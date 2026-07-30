# TODO — status & handoff

_Updated 2026-07-30, after the **first real bring-up on a Bazzite / Fedora-Atomic / GNOME box** and a deep
investigation of the **actual** UniFi auth + VPN flow. This replaces the old pre-Linux work queue._

Read first: [`AGENTS.md`](AGENTS.md) (rules), [`ARCHITECTURE.md`](ARCHITECTURE.md), [`DECISIONS.md`](DECISIONS.md)
(ADR-0001…0015), [`docs/02`](docs/02-vpn-protocol-and-reference-clients.md) (protocol).

## → Current decision & next step (Bazzite session, 2026-07-30 — REVISED)

The capture is **done** (`docs/08`). **This supersedes the earlier "pragmatic, NOT Teleport" note** from the Mac
session — the owner reconsidered once we established that (a) the *newer* native path is un-RE'd by anyone and
auth-walled, while (b) the *older* **Teleport** flow is fully reverse-engineered in **two clean-room references**,
so the scary part (creds → real tunnel through NAT) is already solved and just needs **porting**.

**Decision (ADR-0016): build the VPN engine by porting Teleport to Rust.** In-process userspace WireGuard over
ICE/STUN(+TURN) inside `ternd` — permissive crates only (`boringtun`/`wireguard-rs`, `str0m`/`webrtc-rs`,
`smoltcp`; **never sing-box/GPL**, ADR-0007). References: **`darki73/telepy-cli`** (Python, most complete) and
**`sinnet3000/teleport-client`** (Go). Auth = the SSO-cookie flow (`api/sso/v1/login` + TOTP → `UBIC_AUTH`,
already validated; ADR-0009 revised). This makes ADR-0004 (NetworkManager) a **fallback** for the directly-dialable
case only.

**✅ Gate — VALIDATED 2026-07-30 (GREEN), reference chosen.** telepy's SSO-**directory** path is a dead end for
this account (`ls` empty — the console lives on the newer backend, not legacy `sd-wan/hosts`). **But the Go client
`sinnet3000/teleport-client` connects end-to-end via a `teleport.ui.link/<UUID>` invite:** paired → ICE/STUN
nominated → userspace WireGuard handshake completed → **`curl` through its SOCKS proxy returned HTTP 200 from the
LAN console** (`192.168.1.1` and `192.168.60.1`). So:
- **Entry point = a Teleport invite** (`teleport.ui.link/<UUID>`, generated in the console's Teleport settings),
  **not** the SSO directory. It pairs against the broker `cloudaccess.svc.ui.com/teleport`, saves a reusable
  session, and the invite is single-use.
- **Reference to port = the Go client** (`sinnet3000/teleport-client`), **not** telepy. Stack `wireguard-go` +
  `pion/stun` + `gvisor` (all permissive) → Rust `boringtun` + `str0m` + `smoltcp`.
- **Caveat:** validated from *on* the LAN (ICE chose a direct candidate `192.168.60.1`); a true off-LAN run would
  exercise the reflexive (`<console-WAN-IP>`) / TURN candidates — same code path, not yet tested remotely.
- The newer Identity-Hub/`remote-credentials` chain (doc 08) is now **parked** — the invite/Teleport path works and
  is fully RE'd, so we don't need it.

**Where to work: Bazzite (here).** The Mac's value (real app + capture) is spent; everything left is Linux-runtime
and portable Rust — buildable/testable here (and `tern-core` builds on macOS too if needed).

**Decisions locked (owner, 2026-07-30):** **(a) pure Rust — no interim Go backend** (self-contained, single
language, slower to first tunnel); **(b) system-wide TUN** mode (all apps reach the LAN transparently), which
needs a **one-time privilege grant** (`setcap cap_net_admin` on the daemon, or `pkexec`) — a real revision of the
"no root" stance (ADR-0002/0004) for the Teleport path. A SOCKS-only no-privilege mode may come later as an option.

### Bazzite work queue (port from the Go reference: `sinnet3000/teleport-client`, MIT; re-clone for reference)
1. **✅ Invite parse** — `teleport::Invite::parse` (paste a `teleport.ui.link` link / UUID → validated invite).
2. **✅ Broker auth + transport** — `teleport::secret_to_token` (scrypt→sha512→b64url, known-answer tested),
   `BrokerResponse`/`IceServer`/`ServerInfo` wire types, `Broker::request`/`poll` (wiremock-tested), and
   `BrokerResponse::to_wireguard_config` (bridges a `CONNECT_RESPONSE` → the existing `WireguardConfig`).
   - **✅ Candidate model + ranking (stage ④ core)** — `Candidate`/`PeerDesc` types, `rank_candidates` (host <
     reflex < turn, IPv6 pref), the console's `peer_desc` parsed from `CONNECT_RESPONSE`. Unit-tested.
   - **✅ Live-validated** — `examples/teleport_probe.rs` hit the real broker `/metadata` with our derived token
     → HTTP 200 + the console's info. Proves stages ①–② against reality.
   - **✅ Pairing (redeem invite → session)** — `Broker::pair` = REQUEST_ACCESS → poll ACCESS_GRANTED → `Session`
     {token, secret, device_token}. Wiremock-tested (paused clock). *Consumes the invite when run live.*
   - **✅ STUN wire layer** — `teleport::stun` (`is_stun`/`binding_request`/`parse_xor_mapped_address`), hand-rolled,
     unit-tested (round-trips XOR-MAPPED-ADDRESS).
   - **✅ ICE candidate gathering, LIVE-validated** — `teleport::ice` (`local_candidates` host + `reflexive_candidate`
     via STUN, `is_routable` filter). `examples/teleport_ice_probe` discovered our real public address via
     Cloudflare STUN. So the whole **control plane** (invite → token → pair → session → candidates) is built.
3. **⬜ Connect offer + candidate exchange (LIVE — consumes an invite)** — port `connectAndAwaitResponse`: pick a
   random `stunSecret` (b64 32B; its `stunIntegrityKey` = the secret itself), build the `connect` envelope (our WG
   pubkey + `is_master:false` + local candidates + ice + `secret`), `POST /` with the session token, poll for
   `CONNECT_RESPONSE` (server `peer_desc.candidates` + `wg_pub_key` + tunnel addr). *Ref: Go `main.go`
   `runConnectionAttempt`/`fetchICEConfiguration`.*
4. **✅ ICE/STUN nomination (built; needs a live run)** — `teleport::stun` MESSAGE-INTEGRITY (HMAC-SHA1 keyed by
   `stunSecret`) + `teleport::nomination::await_nomination`: an async loop on the ICE socket that validates the
   console's authenticated Binding requests, replies Binding Success (never sends DATA — sending it reverses the
   role and the console won't activate WireGuard), and tracks the DATA `wait` sequence `[2000,1000,500,250,125]`
   per remote tuple → the completing tuple is the nominated endpoint. *Ref: Go `nomination.go`, `stun.go`.*
   Unit-tested with a local driver socket; **still needs one live run against the console** via the probe below.
5. **✅ Userspace WireGuard over the ICE socket → TUN (built; needs a live run)** — `teleport::dataplane::Tunnel`:
   `boringtun` (BSD-3, our key + the console's key) drives the Noise handshake + transport crypto; a single-task
   pump selects over the ICE socket (ciphertext) and a `tun-rs` TUN device (plaintext), forwarding both ways and
   servicing boringtun's timers. STUN keepalives on the socket are filtered out. Chose `boringtun` **0.7** (shares
   our existing permissive `ring`; 0.6 pins an rc x25519) + `tun-rs` (MIT/Apache — not the WTFPL `tun`);
   BSD-2-Clause added to `deny.toml` for `ip_network*`. Route-setup (AllowedIPs, subnet-subtract) is left to the
   backend. *Ref: Go `wireguard.go`, `tunnel.go`.* Needs the `cap_net_admin` grant (decision b).
   **Live-test both stages:** `cargo build -p tern-core --example teleport_tunnel_probe` then
   `sudo ./target/debug/examples/teleport_tunnel_probe <teleport.ui.link invite | saved-session.json>` — it pairs
   (or reuses a session), connects, nominates, brings up `tern0`, routes the console's `/24`, and pings the gateway.
6. **✅ Integrate (built; connect path pending a live run)** — `TeleportVpn` backend seam (`redeem`/`up`/`down`/
   `is_up`) with a real `tern-linux::teleport::TeleportVpnBackend` (runs `teleport::establish` + iproute2) and a
   `StubBackend` impl for tests. The engine drives the lifecycle: `redeem_invite` (pair → persist session in the
   keyring → bring up), `connect` reconnects a stored session via the Access toggle, `disconnect`/`sign_out` tear
   down + forget, `restore_teleport_session` on startup. Wired through `ternd` (`redeem_invite` D-Bus method +
   startup restore), the CLI (`tern redeem`/`tern import`), and the GUI (invite paste → `RedeemInvite`; tray
   connect/disconnect; console web-UI link). Packaging grants `CAP_NET_ADMIN` via `setcap` (systemd --user can't).
   **Still open:** routing, DNS, and the drives path (`gvfs.rs`, SMB over the tunnel). These want the live
   data-plane confirmation first (the `teleport_tunnel_probe` sudo run) — the backend stays **address-only** until
   then, because a wrong `0.0.0.0/0 dev tern0` would black-hole the host's network, and that must be added under
   supervision, not on an unattended machine.
   **Inferred routing plan** (from docs/02: *full-tunnel by default, client receives routes, the console/gateway
   SNATs* — confirm against the probe's `client_ip`/`dns`/`udp_echo` output): the console assigns a v6 ULA overlay
   (`fd37::x/120`) **and** a v4 `client_ip`; assign *both* to `tern0`, then for full-tunnel add `0.0.0.0/0` +
   `::/0` via `tern0` using the wg-quick trick — a host route to the **nominated endpoint** via the real gateway
   first (so the WireGuard underlay doesn't loop through the tunnel), then split the default into `0.0.0.0/1` +
   `128.0.0.0/1` (or an fwmark + `suppress_prefixlength` rule). Split-tunnel (LAN-only) is the safer first target:
   route just the console's LAN subnet(s). DNS = `resolvectl dns tern0 <dns_addrs>` + `domain tern0 ~.` once routes
   carry the DNS server. All of this belongs in `tern-linux::teleport` `up()` (iproute2), gated on the live model.
7. **⬜ (Later) off-LAN validation** — run from a remote network to exercise the reflexive/TURN path (the live
   validation so far was on-LAN, so only the direct candidate was tested).

**Fallback (only if needed):** the console's built-in **WireGuard Server** → export `.conf` → `tern-linux/src/nm.rs`
(NetworkManager). Now lower priority since the Teleport invite path is validated; keep as the direct-dial option.

_Mac cleanup done: mitmproxy + its CA removed, system proxy off, captured traffic (secrets) discarded — none of it
ever entered the repo._

---

## TL;DR

- **UPDATE (capture done, 2026-07-30) → [`docs/08-live-capture-findings.md`](docs/08-live-capture-findings.md).**
  The real control-plane API was captured (macOS app via mitmproxy). **Login + connect cert-pin** (can't MITM);
  the API calls don't, so we got the post-auth chain (`Identity-Hub` device JWT → `user-token` →
  `cloudaccess…/ids/remote-credentials` → AWS creds + **Cloudflare TURN** + **`directAccessDomain`**). Net: the
  One-Click **data plane is ICE/TURN + userspace WireGuard, not a plain NetworkManager config** (revises
  ADR-0004); the remaining unknown is how the device JWT is first minted — get it by **client-side probing**
  (pinning doesn't apply to a real client). Full map + next steps in doc 08. The rest of this file is prior context.
- The app now **builds, installs, and runs natively on Bazzite** — `ternd` + `tern` + `tern-gui` + tray, under a
  systemd `--user` unit, talking over D-Bus. A real **GUI-crash bug was found and fixed** (it had never been run
  daemon+GUI together before).
- The **auth + VPN flow is now understood end-to-end.** The old design was *half right*: system-browser SSO and a
  UCS `vpn/session` call are real, but the account is **consumer UniFi Identity (UID)**, not the enterprise OAuth
  app the placeholders assumed, and the real onboarding is an **invite → device-credential** bootstrap.
- **Exactly one thing blocks a working One-Click flow: a single traffic capture of the real macOS _UniFi Endpoint_
  app** to pin the UCS enrollment API (`code → credential → vpn/session`). Everything else is scoped and ready.
- **The product's value is the flow**, not raw WireGuard. Importing a hand-made WireGuard `.conf` works today but is
  explicitly *not* the goal — anyone can do that in 5 minutes. Do **not** ship that as "the app."

---

## What works now on Bazzite (verified this session)

- `cargo build/clippy/test` green for the **whole workspace including `tern-gui`** on Linux. 30 tests pass.
- `./packaging/install-local.sh` installs into `~/.local`; `systemctl --user enable --now tern.service` runs the
  daemon; `tern status` → **"Not signed in"**; D-Bus interface (`Snapshot`/`Connect`/`StartSignIn`/…/`Changed`)
  responds; `tern-gui` opens a window + tray and coexists with the daemon.
- `tern-gui` now **captures the `identity-standard://` deep-link** it's launched with (logs it to
  `$XDG_RUNTIME_DIR/tern-deeplink.log`) — the entry point for the invite flow.
- **Login is validated against a real account** via password + TOTP (see below).

### Build environment — the atomic-OS gotcha (don't relearn this the hard way)
Bazzite ships GTK4/libadwaita **runtime** `.so`s in `/usr/lib64` (GNOME needs them) but **no dev headers / `.pc`
files**, so the GUI won't compile against the base image. Get the toolchain from Homebrew and set:
```sh
export PKG_CONFIG_PATH="/home/linuxbrew/.linuxbrew/lib/pkgconfig:/home/linuxbrew/.linuxbrew/share/pkgconfig:$(brew --prefix xorgproto)/share/pkgconfig"
# brew install gtk4 libadwaita cargo-deny   (rust is already via brew)
```
The `xorgproto` `.pc`s live under its keg's `share/pkgconfig` and aren't symlinked — hence the explicit path
(without it, `gtk4.pc` fails resolving `xproto`/`kbproto`). **Runtime is clean**: `ternd`/`tern` are pure-Rust;
`tern-gui` links GTK by SONAME and resolves against the **system `/lib64`** (versions match), so installed binaries
run with no `LD_LIBRARY_PATH`. Brew is a **build-time-only** dependency.

---

## Uncommitted working-tree changes — COMMIT / REVIEW these first

This session left real changes in the tree that are **not yet committed** (run the clippy+test gate, then commit):

1. **D-Bus name-collision fix (ADR-0015) — KEEP.** The GUI's `GtkApplication` id and the daemon's bus name were
   both `phd.hviid.Tern`, so the GUI aborted at startup (`GDBus…UnknownInterface 'org.gtk.Actions'`). Fix: daemon
   bus/interface → **`phd.hviid.Tern.Daemon`**; the GUI keeps the desktop app-id `phd.hviid.Tern` (new
   `ipc::APP_ID`). Touches `ipc.rs`, `ternd`, `tern-cli`, `tern-gui`, the systemd unit, the D-Bus activation file
   (renamed `phd.hviid.Tern.Daemon.service`), `install-local.sh`, and the Flatpak manifest. Documented as ADR-0015.
2. **Deep-link capture in `tern-gui` — KEEP.** Reads the `identity-standard://…` URI from argv, logs + persists it.
   This is the real entry point for the invite flow; grow it into a proper handler (see plan below).
3. **`crates/tern-core/src/auth.rs` — RECONSIDER / likely REVERT.** I pointed `AuthConfig` at
   `sso.ui.com/oauth2/authorize` + `/oauth2/token` (confirmed real via OIDC discovery). **But that DOT/OAuth server
   is the enterprise "SSO Apps" feature — the WRONG system for a consumer UID account** (it rejects any client_id
   we have). The real flow is the invite/UCS path below. Keep the endpoint constants as documented reference, but
   the browser-OAuth-with-client_id approach in `auth.rs`/ADR-0009 does **not** apply to this account — don't build
   on it. (The PKCE/loopback *mechanism* code is still fine and reusable if a real OAuth client ever appears.)

---

## The real auth + VPN flow (corrected — this is the core of the handoff)

### Account reality
The target is a **consumer UniFi Identity (UID)** account (personal ui.com login, One-Click VPN into the owner's
own console), **not** UID Enterprise. Auth is on `sso.ui.com`; the UID/VPN backend base comes from the invite's
`oa` param (a `…svc.ui.com` host). tern's `ucs.rs` skeleton already targets this UCS shape (docs/02 UPDATE).

### Login — three real options (ranked)
1. **Invite → device credential (the real client's method, and the goal).** How the genuine UniFi Endpoint app
   authenticates. Capability-based, per-device, no passkey ceremony, no over-privileged key. **Blocked only on the
   capture** (below). *Build this.*
2. **Password + TOTP** — `POST sso.ui.com/api/sso/v1/login {user,password}` → `499` + `UBIC_2FA` → resubmit
   `{user,password,token}` (6-digit TOTP) → `200` + `UBIC_AUTH` cookie. **VALIDATED end-to-end this session.**
   No `client_id`. **Cannot do passkeys** (owner uses passkeys), so it's a fallback, not primary. Reference impl:
   MIT `darki73/telepy-cli` `adapters/credentials/sso.py`.
3. **Device-code browser+poll (passkey-capable)** — `POST /api/sso/v1/login/token/setup` → short code; poll
   `POST /api/sso/v1/login/token/poll {token}` (`202 {"status":"pending"}` → auth). Passkey happens in the browser.
   The **approval binding wasn't pinned** (the obvious page, `/security/suspicious/confirm`, is the *email* flow —
   "Invalid link"). Would also need a capture to finish. Lower priority than #1.

### VPN provisioning (the "magic")
Consumer One-Click = the **UCS `vpn/session`** flow (docs/02 UPDATE): the enrolled device calls the UCS API
(`…/proxy/ucs/public/user/api/v1/vpn/session`) and gets a **standard WireGuard peer config**, which tern hands to
**NetworkManager** (ADR-0004, `tern-linux/src/nm.rs`). Note: the *macOS app* runs the data plane in userspace
(wireguard-go + sing-box + gVisor netstack + WebRTC — see the fingerprint) for NAT traversal, but the `vpn/session`
response is a normal WG config, so NetworkManager is a legitimate simpler backend **as long as the console has a
reachable endpoint** (the owner's does; there's also a plain "WireGuard Server" on the gateway).

### The invite deep-link (fully captured — no further RE needed here)
- macOS app bundle id `com.ui.uid.standard-desktop`, URL scheme **`identity-standard`** (matches tern's `.desktop`).
- The invite email/link → an `identity.ui.com/s/invitation?...` landing page whose **only** job is to hand off to
  the app via a deep link of the form:
  ```
  identity-standard://identity-standard?url=<url-encoded https invite URL>
  ```
  where the inner URL carries `code=<CODE>&id=<uuid>&oa=<https base>&org=<slug>&src=ucs&type=code&v=<ver>`.
- The identity.ui.com landing app has **no enrollment logic** (confirmed by reading its JS) — the real app does the
  `code → credential → vpn/session` work over TLS to the `oa` host. That's what the capture is for.

---

## Standards mapping (read the capture through this lens — it's not bespoke magic)

None of this is off-menu. It's a **ZTNA device-onboarding stack** (the Tailscale / Cloudflare WARP / Twingate
shape — Ubiquiti's own docs call it "ZTNA"), assembled from standard pieces. Recognising them turns the capture
from blind RE into *confirming a mapping*:

1. **Login / identity = OAuth 2.0.**
   - `sso.ui.com/oauth2/*` is literal **OIDC — Authorization Code + PKCE** (RFC 6749 + RFC 7636; `S256` confirmed).
     (This is the *enterprise SSO-Apps* server — needs a client_id we don't have; not the consumer path.)
   - The app-onboarding code flow (`POST /api/sso/v1/login/token/setup` → short code; poll
     `POST …/login/token/poll` → `202 {"status":"pending"}` until authorized) is the **OAuth 2.0 Device
     Authorization Grant, RFC 8628**, near beat-for-beat: setup ≈ device-authorization endpoint (issues
     `device_code`/`user_code`), poll ≈ token endpoint polled with the device_code returning
     `authorization_pending`. It's a **bespoke variant** (custom JSON `{"status":"pending"}` vs RFC 8628's
     `{"error":"authorization_pending"}`; the "enter this code in your Endpoint" UX = the `user_code`), but the
     semantics are RFC 8628. **Expect/implement it as device flow — look for `interval`, `expires_in`, pending.**
2. **Device credential = public-key-bound token / device enrollment.** `identity/public_key` +
   `credential/{device,confirm,download}` = *device registers a keypair → gets a credential bound to it* —
   the **DPoP (RFC 9449) / mTLS-bound-token (RFC 8705) / cert-enrollment (EST, RFC 7030)** family, and exactly how
   a WireGuard control plane onboards a node (register node pubkey → receive config).
3. **Control-plane signing = AWS SigV4 + Cognito.** The bundled AWS SDK (Cognito, SigV4, AWSIoT) means some calls
   are **SigV4-signed with Cognito temporary credentials** (session/cookie → Cognito creds → signed requests), and
   signaling rides **AWS IoT MQTT**. (This is explicit in the MIT `telepy-cli` consumer-Teleport reference.)
4. **Coordination → data plane = WireGuard (+ ICE/STUN in the Teleport variant).** A coordination service returns a
   **WireGuard** peer config (Noise/Curve25519). The consumer *Teleport* path adds **ICE/STUN (RFC 8445 / 5389)**
   for NAT traversal (the WebRTC bits in the fingerprint); the UCS `vpn/session` path returns a directly-usable
   config when the console has a reachable endpoint (so NetworkManager suffices — no ICE needed).

**One-liner:** OIDC + an RFC-8628-shaped device enrollment + a key-bound device credential + a WireGuard
coordination server, glued with AWS SigV4/IoT.

**So when reading the capture, expect to find (and map):**
- `login/token/setup` / `poll`  ↔  RFC 8628 device authorization + token poll,
- `identity/public_key` + `credential/*`  ↔  "upload my WG public key, get a bound credential",
- `vpn/session`  ↔  "authenticated with that credential, give me a WireGuard peer config".

**Caveats (stay honest):** it's *shaped like* these RFCs, not literally them — exact param names / JSON bodies are
bespoke and still need the capture; and knowing the pattern does **not** remove the capture (paths are auth-walled).

---

## ⛔ THE blocker → the one task that unblocks everything: capture the real app

We need **one** traffic capture of the genuine **macOS _UniFi Endpoint_ app** (`/Applications/UniFi Endpoint.app`,
native SwiftUI + AFNetworking + AWS SDK — **not** Electron) redeeming an invite, to read the enrollment calls.

**Is this API documented / RE'd anywhere? No — confirmed this session.** Ubiquiti's docs are user-facing only
(no API for One-Click VPN / UID). Community clients cover the **Network controller API** (`py-unifi`,
`unifi-controller-api`, `uchkunr/unifi-best-practices`) or the **consumer _Teleport_** flow
(`darki73/telepy-cli`, MIT — different flow: SSO cookie → SigV4 → `cloudaccess.svc.ui.com` → ICE/STUN userspace
WG). **Nothing documents the UID/UCS invite→credential→`vpn/session` API.** Direct probing is dead too:
`enterprise.svc.ui.com` (and `api-gw.uid.df.ui.com`) return **404 to every unauthenticated path** (the gateway
hides endpoints behind auth). So the capture is genuinely the only way in — don't burn time re-searching.

**Method (native app ⇒ proxy + trusted CA; `SSLKEYLOGFILE` won't work):**
1. On the Mac, install an intercepting proxy — **Proxyman** is easiest: `brew install --cask proxyman`, then
   *Certificate → Install for this Mac → Trust* (auto system-proxy + keychain trust). (mitmproxy works too.)
2. Enable TLS decryption for `*.ui.com` and `*.svc.ui.com`.
3. Relaunch UniFi Endpoint and **redeem a fresh invite** (open the `identity-standard://…` link).
4. Capture every request to `*.svc.ui.com` / `identity.ui.com` / `sso.ui.com` during redeem: **method, URL, headers,
   request body, response body**. The ones that matter: identity/credential enroll, `public_key` upload,
   `credential/{device,confirm,download}`, and `vpn/session`.
5. **If the app cert-pins** (TLS errors through the proxy): escalate to a **Frida** hook disabling AFNetworking's
   `AFSecurityPolicy` pinning. Try the plain proxy first — AFNetworking usually doesn't pin by default.

Redact bearer tokens/keys before sharing — only the **paths + JSON shapes** are needed. **No secrets in the repo.**

---

## Implementation plan once we have the capture

All of this is well-scoped; the capture only fills in exact paths/shapes.

1. **`tern-core::ucs`** — set the base host from the invite `oa`; implement `enroll(code) → credential`
   (generate WG keypair via `wg.rs`, upload public key, confirm), store the credential in the keyring
   (`SecretStore`), then `vpn/session` → parse the WireGuard config into the existing model.
2. **`tern-core::auth` (new invite path)** — parse `identity-standard://identity-standard?url=…`, extract
   `code`/`oa`/`org`, drive enrollment. Wire the `tern-gui` deep-link capture (already present) → a `ternd`
   method (`RedeemInvite(uri)`), plus a `tern login --invite <url>` CLI path.
3. **`ternd`** — new D-Bus method for invite redemption; persist the device credential; restore-session uses the
   credential (not a bearer) to re-mint `vpn/session` on connect. Keep the `Changed` snapshot contract.
4. **`tern-linux::nm`** — verify the `vpn/session` WG config imports cleanly (`nmcli … import wireguard` / zbus) and
   toggles password-free as a user-owned connection (ADR-0004). This is TODO's original task 3, now unblocked by a
   real config.
5. Then the rest of the original queue: **drive mounting** (`gvfs.rs` + keyring creds), **real reachability**
   (`reach.rs`, probe the drive host:445), **auto-reconnect**, **multi-site picker**, **Flatpak** (D-Bus NM
   backend, ADR-0014), **release CI + tap** (owner-gated).

---

## Rejected / fallback paths (so nobody re-explores them)

- **Enterprise OAuth (`sso.ui.com/oauth2/authorize`)** — real DOT/OIDC server (endpoints + scopes
  `read billing openid introspection ui`, PKCE S256 confirmed), but it's the **third-party "SSO Apps"** feature and
  needs a registered `client_id` we don't have. Wrong system for a consumer account.
- **Official API key (Site Manager, `api.ui.com` + connector proxy)** — *works* and can even reach the console's
  WireGuard config remotely via the legacy admin API… **but the only key that can is a full-admin key** (no
  "read-my-VPN-only" scope exists). **Owner rejected it: too much power for a client.** Fine for read-only recon,
  not as tern's auth.
- **Console's built-in "WireGuard Server" → manual `.conf` import** — works today, fits NetworkManager, zero
  capture. **Explicitly a fallback / dev aid, not the product** (misses the flow that gives tern its value).

---

## Reference: confirmed public endpoints (safe to keep)

| Purpose | Endpoint |
|---|---|
| SSO login (password+TOTP) | `POST https://sso.ui.com/api/sso/v1/login` (`{user,password[,token]}`) → `UBIC_AUTH` |
| Device-code login | `POST /api/sso/v1/login/token/setup` → code; `POST /api/sso/v1/login/token/poll {token}` |
| OIDC discovery (enterprise SSO-Apps) | `https://sso.ui.com/oauth2/.well-known/openid-configuration` |
| UID/VPN base | from invite `oa` param (a `*.svc.ui.com` host) |
| UCS VPN session (target) | `POST {oa}/proxy/ucs/public/user/api/v1/vpn/session` → WireGuard config |
| Deep-link scheme | `identity-standard` (`.desktop` `x-scheme-handler/identity-standard`) |

Owner-specific infra (console IDs, org slug, DDNS, WireGuard server subnets) is intentionally **not** in the repo.

---

## Rules that bite (full list in AGENTS.md)
- **Commit gate:** `cargo clippy --workspace --all-targets -- -D warnings` **and** `cargo test --workspace` must
  pass. Also `cargo deny check licenses bans sources`. No `cargo fmt` gate.
- **Commits:** Conventional Commits, lowercase, **no `Co-Authored-By`/AI attribution**; author as the owner.
- **Licensing:** MIT; never in-process-link sing-box or libsmbclient; no OpenSSL (rustls only).
- **No secrets in the repo.** Keep tokens/keys/real invite codes/console IDs out.
- **User-facing text:** plain language only (docs/05); a test forbids jargon in error titles.
