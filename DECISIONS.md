# Decision Log

Append-only log of architecture/design decisions, with the options weighed and the reasoning — so a later
reader (or the owner returning) can see *why*, not just *what*, and overrule with context. Newest at top of
each section. Format per entry: **Context → Options weighed → Decision → Why → Revisit if**.

Status legend: 🟢 firm · 🟡 provisional (revisit with real Linux testing) · 🔵 naming/cosmetic (owner may override)

---

## ADR-0001 — Language & runtime 🟢
**Context:** Owner prefers Rust (not forced); all four reference repos (grove/temper/amdl/dotsync) are Rust
Cargo workspaces. Target is a GNOME-native desktop app + background networking.
**Options:** Rust; Go (sing-box/wireguard-go are Go — easy tunnel reuse, but GTK bindings are weaker and it
pulls GPL sing-box); C/Vala (native GNOME but slow to build, small ecosystem).
**Decision:** **Rust**, async on **tokio**, HTTP via **reqwest + rustls** (no OpenSSL — cleaner licensing &
static linking), **serde** for models, **zbus** for D-Bus.
**Why:** Matches owner conventions; best-in-class GTK4 bindings (gtk4-rs); permissive crates; rustls avoids the
OpenSSL license/build friction; one language across daemon/CLI/GUI.
**Revisit if:** a Linux-only blocker forces a Go tunnel component (would run it out-of-process anyway).

## ADR-0002 — Process architecture: background service + thin clients 🟢
**Context:** The app must hold a login session, auto-reauthenticate, auto-connect at startup, auto-reconnect,
and mount/unmount drives on reachability changes — i.e. it must keep working with **no window open**.
**Options:**
1. *Monolith* — one GUI process does everything. Easiest. But state dies when the window closes; no headless
   path; hard to test logic without a display; awkward to reconnect on login.
2. *GUI + privileged root helper* — needed only for a custom kill-switch/boot tunnel. Heavy; forces native
   packaging; more attack surface.
3. *Background user service + thin clients (tray/GUI/CLI)* — the service owns session + orchestration and
   drives NetworkManager (VPN) and GVfs (mounts); clients are views over a D-Bus API. (Mullvad/Tailscale shape.)
**Decision:** **Option 3**, but as **one binary, role-selected** — `ternd` (service role) + `tern` (CLI client)
+ `tern-gui` (tray/GUI client) sharing `tern-core`. Native ships a **systemd `--user` unit** for the service;
under **Flatpak** the GUI spawns the service as a child and uses the **Background portal** to persist.
**Why:** Robustness (survives GUI restart, reconnect on login), a single source of truth for session state,
and — critically for *this* effort — a **headless core+CLI I can build and test on macOS today** without a
display or Linux. We are explicitly not choosing the easiest (monolith); the daemon split is worth the IPC cost.
**Revisit if:** Flatpak background-service lifecycle proves too painful → fall back to GUI-embeds-service (same
`tern-core`, just no separate process) without changing the core.
**Note:** No *privileged* helper — all privilege is delegated (ADR-0004/0005), so this service runs unprivileged.

## ADR-0003 — Crate layout 🟢
**Decision:**
```
crates/
  tern-core     # lib, platform-agnostic: SSO/UCS auth, API client, WireGuard config model,
                #   reachability logic, state machine, error→user-message taxonomy, config/persistence,
                #   and backend TRAITS (VpnBackend, MountBackend, SecretStore, Reachability). Fully testable on macOS.
  tern-linux    # lib, cfg(linux): trait impls — NetworkManager (zbus), GVfs (gio/zbus), libsecret (oo7).
  ternd         # bin: the background service; owns core + selected backends; exposes a D-Bus session API.
  tern-cli      # bin: thin control client (login/connect/status/drives) — also drives core+stub on macOS.
  tern-gui      # bin: relm4 + libadwaita GUI + ksni tray; D-Bus client of ternd.
```
**Why:** Clean seam between portable logic (testable here) and Linux system integration (testable on Bazzite).
A `StubBackend` in `tern-core` (behind a test/dev feature) lets the CLI exercise the full flow on macOS.
**Revisit if:** crate count feels heavy — `tern-linux` could fold into `ternd` behind `cfg`.

## ADR-0004 — VPN control: delegate to NetworkManager 🟠 REVISED → see ADR-0016
> **REVISED 2026-07-30 (after the live capture, doc 08).** One-Click has **no dialable endpoint** — it's
> ICE/TURN-relayed userspace WireGuard — so NetworkManager can't carry it. ADR-0016 makes the **primary** VPN
> path an **in-process userspace WireGuard-over-ICE/TURN** engine (ported from telepy). **NetworkManager is
> demoted to a *fallback*** for the directly-dialable case only (the console's built-in WireGuard Server, or a
> reachable `id.ui.direct`/DDNS endpoint). The rest of this ADR still applies to that fallback.
**Context:** Bringing a WireGuard tunnel up needs root/CAP_NET_ADMIN. One-Click VPN config is fetched per
session from the UCS API (doc 02), not a static file.
**Options:** own root helper + `wg`; `wg-quick` via `pkexec` (broad, ugly); userspace boringtun in-process
(no root but reimplements routing/DNS, weaker); **NetworkManager** native WireGuard over D-Bus.
**Decision:** **NetworkManager via zbus**, tunnel stored as a **user-owned connection** so activate/modify is
password-free for the logged-in user (polkit `network-control`/`settings.modify.own = yes`). Split-tunnel via
per-peer `allowed-ips`.
**Why:** Least privilege (our process stays unprivileged), NM already runs privileged and persists profiles,
and it's the Flatpak-friendly path (`--system-talk-name=org.freedesktop.NetworkManager`). Bazzite ships NM.
**Revisit if:** a host lacks NM (systemd-networkd-only) → optional boringtun-in-`ternd` fallback behind a trait.

## ADR-0005 — Drive mounting: GVfs default, kernel cifs optional 🟡
**Context:** Selective per-drive SMB auto-mount, reachability-gated (LAN or over VPN). SMB needs L3
reachability — remote-without-VPN relay is out of scope (proprietary, doc 01).
**Options:** kernel `mount.cifs` (root, real mountpoint, all apps see it) vs **GVfs `gio mount smb://`**
(userspace, no root, native Files integration, but non-GIO apps only via the FUSE bridge).
**Decision:** **GVfs by default** (no privilege, best GNOME integration, keeps GPL-3 libsmbclient out of our
process), with kernel `mount.cifs` as an opt-in "make drives visible to all apps/CLI" mode later.
**Why:** Matches the "just works in Files" desktop expectation with zero privilege; libsmbclient stays
out-of-process (licensing, ADR-0007). **Revisit** after testing GVfs SMB reliability + keyring persistence on
Bazzite (known flaky spots).

## ADR-0006 — GUI toolkit: relm4 on gtk4-rs + libadwaita 🟢
**Options:** raw gtk4-rs (native, more boilerplate); **relm4** (MVU on gtk4-rs — native look, less boilerplate,
async built in); Tauri (web UI, not Adwaita-native, WebKitGTK skew); Iced/libcosmic (COSMIC look, not GNOME);
Slint (own renderer + GPL/attribution/paid tri-license — rejected on licensing).
**Decision:** **relm4 + gtk4-rs + libadwaita**; tray via **ksni** (pure-Rust StatusNotifierItem, Unlicense).
**Why:** Only stack that yields a true Adwaita/GNOME-HIG look; fastest for a small tray-driven multi-panel app;
all permissive. gtk4-rs+libadwaita also *build and run on macOS* (via `brew install gtk4 libadwaita`), so the
GUI is partly testable here.
**Revisit if:** relm4 friction outweighs benefit → drop to raw gtk4-rs (same widgets).

## ADR-0007 — Licensing posture: permissive (MIT), keep GPL out-of-process 🟢
> **REAFFIRMED 2026-07-30, now load-bearing.** ADR-0016 ports a userspace WireGuard + ICE stack **in-process** —
> which is exactly where the GPL trap bites. The macOS app uses **sing-box (GPL-3)** for this; we must **not**.
> Use only permissive primitives: **`boringtun`** (BSD) / `wireguard-rs`, **`str0m`** or `webrtc-rs` (ICE/STUN/
> TURN, MIT/Apache), **`smoltcp`** (userspace netstack, 0BSD/MIT). This constraint is *the* reason we port from
> the (MIT) `telepy-cli` design rather than embed any Ubiquiti/sing-box code. Enforced by `cargo deny`.
**Decision:** App code **MIT** (matches temper/amdl/dotsync). Get userspace WireGuard from permissive
primitives if ever needed (wireguard-go/MIT, boringtun/BSD, gVisor/Apache) — **never** in-process-link
**sing-box (GPL-3)** or **libsmbclient (GPL-3)**; reach SMB via GVfs/exec. Reuse the two **MIT** reference
clients' SSO code with attribution. (Full analysis: doc 04.)
**Why:** Ubiquiti embeds GPL-3 sing-box for convenience; we don't need it and avoid viral copyleft.
**Revisit if:** we decide to embed sing-box for signaling parity → then app must go GPL-3 (avoid).

## ADR-0008 — Distribution: Flatpak primary (Bazzite), native + tap secondary 🟡
**Context:** Owner's test box is Bazzite (Fedora Atomic/uBlue) — immutable, Flatpak-first, rpm-ostree.
**Decision:** **Flatpak/Flathub the primary channel** (delegating VPN→NM, SMB→GVfs makes a real sandbox
viable); **native `.rpm`/`.deb` + AUR** in parallel for non-NM/enterprise hosts; a **Homebrew formula** in the
owner's tap only for any headless CLI (`tern-cli`) — GUI is not a brew fit on Linux. **Do NOT push to
jakobhviid/homebrew-tap yet** (owner instruction).
**Why:** Meets the test machine where it lives; sandbox story is clean with the delegated design.
**Revisit if:** Flatpak can't drive NM/GVfs acceptably on Bazzite → lead with native rpm + a systemd user service.

## ADR-0009 — Auth: system-browser SSO (passkey-compatible), loopback callback 🟠 REVISED → see ADR-0016
> **REVISED 2026-07-30.** For a **consumer** account this browser-OAuth-with-`client_id` model is wrong: the
> `sso.ui.com/oauth2` server is the *enterprise* "SSO Apps" feature and rejects any client we have. The real,
> **validated** path is the telepy-style **SSO-cookie flow** — `POST sso.ui.com/api/sso/v1/login`
> (`user`+`password`+TOTP) → `UBIC_AUTH` → short-lived cloud creds — which ADR-0016 adopts. The **PKCE + loopback
> mechanism code stays** (generic, unit-tested, reusable), but is not the product path. **Passkey login is
> deferred** (it needs the pinned browser flow + a device-code approval we couldn't pin — TODO/doc 08).
**Context:** The Mac app signs in via the system browser (handles MFA + enterprise SAML/IdP transparently),
then uses the resulting session for the UCS API (doc 02 UPDATE). **Owner note: the login flow often uses
passkeys (WebAuthn/FIDO2).**
**Options:** scripted username/password+TOTP (reference-client style — breaks on SAML/push **and cannot do
passkeys**); an embedded webview (many lack a WebAuthn platform authenticator); **system-browser OAuth** + callback.
**Decision:** **System-browser SSO** (RFC 8252 native-app flow + PKCE). Callback via a **loopback redirect**
(`http://127.0.0.1:<port>/callback`) as the primary mechanism, with the custom scheme
(`x-scheme-handler/identity-standard`) as a fallback. `ternd` owns the ephemeral loopback listener + token
exchange; tokens go to the system keyring (oo7). We never see credentials.
**Why:** Only the real browser gives **passkey/WebAuthn** support (platform authenticator: Touch ID, security
key, phone passkey) — plus MFA + SAML — for free. Loopback is the most reliable callback across desktops and
inside Flatpak (custom-scheme registration is finicky there).
**Revisit if:** the SSO requires a fixed/registered redirect URI disallowing loopback → fall back to the custom
scheme. Confirm the exact redirect + whether the flow is OAuth/OIDC during the Bazzite traffic capture (M7).
**Status:** the browser-launch + loopback-catch + token-exchange **mechanism is built and unit-tested**
(`tern_core::auth` — incl. the RFC 7636 PKCE test vector and a live loopback-capture test). `ternd` exposes
`StartSignIn` (browser flow) with a `CompleteSignIn(token)` fallback; `tern login` and the GUI "Sign in" button
drive it. Only the UniFi authorize/token URLs + `client_id` remain to pin from the M7 capture.

## ADR-0010 — Names & identifiers 🔵 (owner may override freely)
**Decision:** Product codename **"tern"** (a long-migration seabird that returns home — fits roaming access to
your home network); binaries `ternd`/`tern`/`tern-gui`; crates `tern-*`; app-id / D-Bus / Flatpak id
**`phd.hviid.Tern`**. Repo name **`tern`** (github.com/jakobhviid/tern) — matching the owner's product-named
repos (grove/temper/steel); discoverability via the GitHub description + topics (`unifi`, `unifi-identity`,
`wireguard`, `gnome`, `flatpak`). README states "Unofficial UniFi Identity endpoint client — not affiliated
with Ubiquiti," and we use **no** UniFi/Ubiquiti branding/logos (trademark safety, doc 04).
> App-id reverse-DNS uses **`hviid.phd`** (the owner's personal domain, per owner) so it is
> **Flathub-verifiable**. (`hviid.cloud` is the owner's home-setup domain — not used for this app.) Release
> intent (owner-confirmed): Flatpak → **Flathub**; the `tern-cli` bottle → **jakobhviid/tap** (gated until go).
**Why:** Nominative-use repo name for discoverability + a distinct product name to avoid trademark in branding.
**Revisit:** entirely the owner's call — grep `tern` / `phd.hviid.Tern` to rename.

## ADR-0011 — Commit & attribution conventions (mirror owner's repos; overrides harness default) 🟢
**Context:** Owner pointed to the sibling repos as canonical. Their `AGENTS.md` carries a hard rule:
Conventional Commits, lowercase imperative subjects, **no trailers**, and **never add AI/assistant attribution**
— author every commit as the repo owner; AI use is disclosed once, in the README. This deliberately overrides
the harness's default `Co-Authored-By` trailer.
**Decision:** Conventional Commits (`feat/fix/docs/chore/refactor/test/ci/perf`, `feat!` = breaking); **no
`Co-Authored-By` / AI trailers**; commit author = `Jakob Hviid, PhD <jakob@hviid.phd>` (author == committer);
a single **AI-disclosure** paragraph in the README. Version auto-derived from commit history (baseline 1.0.0)
via the owner's awk scheme (`feat`→minor, `feat!`/BREAKING→major, else→patch).
**Why:** The owner explicitly instructed me to follow how they normally work; their repos are unambiguous here.
**Revisit:** owner can re-enable trailers at will.

## ADR-0012 — Docs: keep the research `docs/` **and** add the owner's top-level docs 🟢
**Context:** Owner's repos use top-level uppercase docs (`AGENTS.md`, `WORKFLOWS.md`, `ARCHITECTURE.md`, README
with an "AI disclosure" section) and **no `docs/` folder**. This project already has a substantial research
`docs/` (teardown, protocol, licensing, UX) that is a genuine asset.
**Decision:** Keep `docs/` (research/reference) **and** add `AGENTS.md` + `CLAUDE.md`(@AGENTS.md) +
`WORKFLOWS.md` + `ARCHITECTURE.md` at the root in the owner's style; README links to `docs/`. `DECISIONS.md`
is our running decision log (the owner asked for one explicitly).
**Why:** Don't discard useful research to fit a template; adopt the template's agent/workflow docs on top.
**Revisit:** owner may prefer to fold `docs/` into top-level files.

## ADR-0013 — CI shape: clippy+test gate now; Flatpak + CLI-tap release later 🟡
**Context:** Owner's release CI cross-compiles static **musl** binaries + darwin and pushes Homebrew bottles.
That fits a pure CLI. Our GUI links **GTK4/libadwaita** (glibc, dynamic) — musl-static cross-compile is not
viable for the GUI, and Flatpak is the right desktop channel (ADR-0008).
**Decision:** Adopt the owner's **clippy `-D warnings` + `cargo test`** gate verbatim (workspace). For
releases: **Flatpak** for the GUI (`tern-gui`), and the owner's **musl bottle + tap template** only for the
headless **`tern-cli`** (which *can* build static). Reuse the awk version-stamp + release-on-push-to-main model.
Tap push stays **disabled** until the owner provisions secrets and says go (owner said don't push to the tap yet).
**Why:** Keep the parts of their pipeline that fit; diverge only where the GUI/Flatpak reality demands, and say why.
**Revisit:** once building on Bazzite, confirm the Flatpak build + the CLI bottle both work end-to-end.

## ADR-0014 — Backend delivery: CLI-exec now (native), D-Bus needed for Flatpak 🟡
**Context:** `tern-linux` backends shell out to `nmcli` / `gio` / `secret-tool`. That works for **native**
installs (immediate Bazzite testing via source/brew/rpm) but **not inside a Flatpak sandbox** — `nmcli` isn't
in the GNOME runtime (and `secret-tool` may not be), while `gio` is. Flathub is the eventual release target.
**Decision:** Ship the **CLI-exec backends now** for native/immediate testing. **Before the Flathub release,
port the VPN backend to D-Bus** (NetworkManager via `zbus` on the system bus + `--system-talk-name`),
secrets to the **Secret Service D-Bus / Secret portal**, and keep mounts on **GVfs** (`gio` is in the runtime;
otherwise GVfs D-Bus). The engine's trait seams (`VpnBackend`/`MountBackend`/`SecretStore`) make each a
drop-in swap — **no `tern-core` changes**.
**Why:** D-Bus backends are unprivileged *and* Flatpak-compatible (the right end-state); CLI-exec is the
fastest correct path for native testing today, and it's verifiable on macOS (pure `std::process`).
**Revisit:** implement + test the D-Bus NetworkManager backend on Bazzite → the Flatpak becomes fully
functional. Until then, the Flatpak manifest is scaffolding (VPN won't work in-sandbox with the nmcli backend).

## ADR-0015 — Daemon bus name is a sub-name of the app-id (`phd.hviid.Tern.Daemon`) 🟢
**Context:** First real Bazzite bring-up (daemon + GUI running together on one session bus, which had never
happened before — on macOS they aren't co-run). The GUI aborted at startup with
`GDBus...UnknownInterface: 'org.gtk.Actions'`. Cause: `ternd` owns the well-known name `phd.hviid.Tern`
(ADR-0010), and the GUI's `adw::Application` used that **same** string as its `application_id`. A
`GtkApplication` always tries to own its app-id on the session bus; finding it already owned, GApplication
assumed a *primary GApplication instance* lived there and tried to talk `org.gtk.Application`/`org.gtk.Actions`
to `ternd` (a plain zbus service) — which doesn't implement those — so registration failed and the GUI quit.
**Options:** (a) `G_APPLICATION_NON_UNIQUE` on the GUI — a hack; loses single-instance/actions/portal, and
Flatpak wants the GUI to actually **own** the app-id; (b) give the GUI a different app-id like
`phd.hviid.Tern.Gui` — but the Wayland `app_id`/WM-class then wouldn't match the `.desktop`/icon (broken icon)
and Flatpak requires the primary GApplication id == the Flatpak id; (c) **keep the app-id on the GUI, move the
daemon to a sub-name.**
**Decision:** **(c).** The **desktop app-id stays `phd.hviid.Tern`** (`.desktop`/icon/metainfo/Flatpak id **and**
the GUI's `GtkApplication` id → Wayland app_id). The **daemon's D-Bus service + interface name becomes
`phd.hviid.Tern.Daemon`** (a sub-name of the app-id); object path stays `/phd/hviid/Tern`. New constant
`ipc::APP_ID` (GUI/tray) is now distinct from `ipc::BUS_NAME`/`INTERFACE` (daemon). Updated: `ternd`,
`tern-cli`, `tern-gui`, the systemd unit `BusName=`, the D-Bus activation file (renamed
`phd.hviid.Tern.Daemon.service`), `install-local.sh`, and the Flatpak manifest's activation block.
**Why:** This is the standard GNOME/Flatpak split — the user-facing app owns the app-id; background helpers take
sub-names. It removes the collision **and** improves the Flatpak story (a sandbox auto-owns app-id sub-names, so
no extra `--own-name` is needed). Amends ADR-0010's "D-Bus id `phd.hviid.Tern`": the *app/Flatpak* id is
unchanged; only the *daemon's* bus/interface name gains the `.Daemon` suffix.
**Verified:** on Bazzite/GNOME — daemon (`phd.hviid.Tern.Daemon`) and GUI (`phd.hviid.Tern`) now coexist on the
session bus; `tern status` and the GUI both render "Not signed in"; the GUI no longer aborts at registration.
**Revisit if:** the owner renames the product (grep `phd.hviid.Tern`); if a future single-binary/monolith mode
(ADR-0002 fallback) runs the engine inside the GUI process, the split is moot (one process owns the app-id).

## ADR-0016 — VPN data plane: port Teleport (userspace WireGuard over ICE/TURN) to Rust 🟡
**Context:** The live capture (doc 08) settled how One-Click actually works: it is **Teleport-shaped** — no
dialable endpoint, a coordination call returns **TURN creds + a `directAccessDomain`**, and the tunnel is
**userspace WireGuard carried over ICE/WebRTC** (direct when reachable, Cloudflare-TURN-relayed when NAT'd).
The *newer* native chain (`Identity-Hub` JWT → `remote-credentials` → Cloudflare TURN) is **undocumented and
un-RE'd by anyone** (confirmed by search); the *older* consumer-**Teleport** chain is fully reverse-engineered in
two clean-room references — **`darki73/telepy-cli`** (Python, most complete: SSO+MFA → cloud creds → console
directory → AWS-IoT-MQTT+HTTPS signaling → ICE/STUN → from-scratch WireGuard + userspace TCP/IP or TUN) and
**`sinnet3000/teleport-client`** (Go: in-process WG, STUN, SOCKS). Owner decision: **Teleport is good enough**
for the goal; **Python is not** shippable here → **port it to Rust.**
**Options weighed:** (a) build the *newer* Identity-Hub/Cloudflare path — no reference, auth-walled, moving target
(rejected); (b) embed sing-box/wireguard-go out-of-process — GPL + heavy (rejected, ADR-0007); (c) shell out to
telepy (Python runtime dep in a Rust/Flatpak app — rejected); (d) **port the Teleport design to Rust with
permissive crates** (chosen).
**Decision:** Implement the VPN engine **in-process in `ternd`** as a Rust port of the Teleport flow, using
**`boringtun`/`wireguard-rs`** (userspace WG), **`str0m`/`webrtc-rs`** (ICE/STUN/TURN), **`smoltcp`** (userspace
netstack) or a TUN for system-wide, and an MQTT client for signaling. Auth = the SSO-cookie flow (ADR-0009
revised). This **supersedes ADR-0004 as the primary path** (NetworkManager kept only as the directly-dialable
fallback) and **retires the `ucs.rs` `vpn/session`→plain-config assumption** (doc 08: no such endpoint).
**Build order (staged, each independently testable):** ① SSO auth (have it, validated) → ② console directory /
cloud creds → ③ signaling (MQTT+HTTPS, key + ICE-candidate exchange) → ④ ICE/STUN(+TURN) → ⑤ userspace WireGuard
+ netstack → ⑥ wire into the engine/daemon/tray/drives that already exist.
**Why:** The discovery risk is gone (two references document the wire format); what remains is a bounded port with
permissive crates that keeps us MIT. It delivers the *actual* flow (tern's whole reason to exist), not a
hand-rolled `.conf` wrapper.
**Confidence / gate 🟡:** the owner's account was captured on the *newer* chain (doc 08), while the references
implement the *older* Teleport chain. **Before the big port, validate `telepy-cli` connects to the owner's
console today.** If yes → port it wholesale. If the old chain is retired for the account → port telepy's **data
plane** but take the **control plane** from doc 08 (client-side-probe the one gap: how the `Identity-Hub` JWT is
minted). Either way the data-plane port is the same work.
**Revisit if:** Ubiquiti fully retires the Teleport/MQTT signaling for consumer accounts (then only the doc-08
chain remains — port data plane, RE the control plane), or a directly-dialable path (ADR-0004 fallback) turns out
to cover the owner's real need (then this large port may be unnecessary for *this* user).
