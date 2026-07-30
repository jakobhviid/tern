# TODO — status & handoff

_Updated 2026-07-30, after the **first real bring-up on a Bazzite / Fedora-Atomic / GNOME box** and a deep
investigation of the **actual** UniFi auth + VPN flow. This replaces the old pre-Linux work queue._

Read first: [`AGENTS.md`](AGENTS.md) (rules), [`ARCHITECTURE.md`](ARCHITECTURE.md), [`DECISIONS.md`](DECISIONS.md)
(ADR-0001…0015), [`docs/02`](docs/02-vpn-protocol-and-reference-clients.md) (protocol).

### 👉 If you're the agent picking this up (likely on the owner's Mac)
The single thing that unblocks the whole product is a **traffic capture of the real macOS _UniFi Endpoint_ app**
redeeming an invite. **Go straight to [⛔ THE blocker](#-the-blocker--the-one-task-that-unblocks-everything-capture-the-real-app)**,
get the capture, paste it here (redacted), then follow [the implementation plan](#implementation-plan-once-we-have-the-capture).
You are on the right machine for this: the Mac has the app, can run the capture, **and** can build + unit-test
`tern-core`/`tern-cli` (no GTK/D-Bus needed — `cargo test -p tern-core`), so you can implement + test `ucs.rs`
right there. GUI + NetworkManager runtime validation happens back on the Bazzite box (see the build note below).
Everything already discovered — endpoints, deep-link format, why the other paths were rejected — is in this file
so you don't have to re-derive it.

---

## TL;DR

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
