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

## ADR-0004 — VPN control: delegate to NetworkManager 🟢
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

## ADR-0009 — Auth: system-browser SSO, mirror the Mac app 🟢
**Context:** The Mac app signs in via the system browser (handles MFA + enterprise SAML/IdP transparently),
then uses the resulting session for the UCS API (doc 02 UPDATE).
**Options:** scripted username/password+TOTP (reference-client style — breaks on SAML/push); **browser OAuth
+ custom URL-scheme callback** (`x-scheme-handler/identity-standard`).
**Decision:** **Browser SSO + loopback/scheme callback**, tokens in the system keyring (oo7). Adapt the
reference clients only for the post-login UCS calls.
**Why:** Transparently supports MFA/SAML, matches the real client, less brittle, no password handling.
**Revisit if:** the scheme callback is unreliable inside Flatpak → use a localhost loopback redirect instead.

## ADR-0010 — Names & identifiers 🔵 (owner may override freely)
**Decision:** Product codename **"tern"** (a long-migration seabird that returns home — fits roaming access to
your home network); binaries `ternd`/`tern`/`tern-gui`; crates `tern-*`; app-id / D-Bus / Flatpak id
**`dk.jakobhviid.Tern`** (`.dk` fits owner). Repo name **`unifi-endpoint-linux`** (discoverable; clearly
*unofficial*). README states "Unofficial UniFi Identity endpoint client — not affiliated with Ubiquiti," and we
use **no** UniFi/Ubiquiti branding/logos (trademark safety, doc 04).
**Why:** Nominative-use repo name for discoverability + a distinct product name to avoid trademark in branding.
**Revisit:** entirely the owner's call — grep `tern` / `dk.jakobhviid.Tern` to rename.

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
