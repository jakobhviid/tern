# HANDOVER — bootstrap for the next session (human or LLM)

_Written 2026-08-01. Read this, then [`TODO.md`](TODO.md) (task checklist), then [`AGENTS.md`](AGENTS.md)
(the rules — they override defaults). This file is the distilled knowledge of the sessions that got Teleport
working; it exists so that knowledge isn't lost across a pause._

---

## 1. What this project is

**tern** — an unofficial, GNOME-first **Linux client for UniFi Identity's One-Click VPN** ("Teleport"), plus
selective auto-mounting of UniFi Drive (SMB) shares. Not affiliated with Ubiquiti. Clean-room interop.
Rust workspace; the real target is a **Bazzite / Fedora Atomic / GNOME** box (the owner's). MIT-licensed;
**staying MIT is load-bearing** (no GPL/sing-box/OpenSSL in-process — enforced by `cargo deny`).

## 2. Status in one paragraph (THE headline)

The **Teleport VPN works end-to-end — validated LIVE against the owner's real console.** A DNS query to the
remote DNS server (`192.168.1.1`) was answered *through the tunnel*: invite → broker pairing → ICE/STUN
nomination → WireGuard handshake → **bidirectional encrypted app traffic**. All of it is built and wired
through the daemon, CLI, and GUI. **The single open verification** is confirming the *daemon* (not just a
sudo-run probe) can bring the tunnel up — this hinges on a Linux capability fix (§7) that the owner needs to
test on the box with the command in TODO.md. After that: broader routing and drives-over-tunnel (both need
live iteration).

## 3. Build, test, run — and the ONE environment gotcha

- **The gate (AGENTS.md rule 2), run before every commit:**
  `cargo clippy --workspace --all-targets -- -D warnings` **and** `cargo test --workspace`. Also
  `cargo deny check licenses bans sources`. There is deliberately **no `cargo fmt` gate**.
- **GUI build gotcha (Bazzite):** `tern-gui` links gtk4/libadwaita from **Homebrew**, and the X libs need
  `xorgproto`. You must export before building anything that touches the GUI:
  ```sh
  export PATH="/home/linuxbrew/.linuxbrew/bin:$PATH"
  export PKG_CONFIG_PATH="/home/linuxbrew/.linuxbrew/lib/pkgconfig:/home/linuxbrew/.linuxbrew/share/pkgconfig:$(brew --prefix xorgproto)/share/pkgconfig"
  ```
  `tern-core`, `tern-cli`, `ternd`, `tern-linux` build **without** this (no GUI system-libs). `install-local.sh`
  builds/install the CLI+daemon first (no PKG_CONFIG_PATH needed), then the GUI with the brew paths.
- **Install on the box:** `./packaging/install-local.sh` (builds release, installs to `~/.local`, sets
  `setcap cap_net_admin+eip` on `ternd`). Then `systemctl --user restart tern.service`.
  ⚠ **`install-local.sh` does NOT restart the service** — a running old `ternd` keeps serving until you
  `systemctl --user restart tern.service`. This bit us: symptom is `tern redeem` returning a stale message.
- **The live diagnostic probe** (no daemon, needs sudo): `examples/teleport_tunnel_probe`. It runs the whole
  path and prints rich diagnostics (handshake, tx/rx, decrypted inbound packet summaries, host routing). Use it
  to debug the data plane in isolation. Saves the paired session to `/tmp/tern-teleport-session.json` for reuse.
  ```sh
  cargo build -p tern-core --example teleport_tunnel_probe
  sudo ./target/debug/examples/teleport_tunnel_probe <teleport.ui.link invite | /tmp/tern-teleport-session.json>
  ```

## 4. Architecture (crates + the data flow)

```
crates/tern-core   platform-agnostic: auth, UCS API, models, state machine, error→UX taxonomy, config, wg keys,
                   the orchestration ENGINE, the backend TRAITS, and the whole TELEPORT client + data plane.
crates/tern-linux  Linux backend impls: NmVpn (nmcli), GVfs mounts, keyring — AND teleport::TeleportVpnBackend
                   (runs the in-process data plane + configures tern0 with iproute2).
crates/ternd       the background service: owns the Engine, serves a session-bus D-Bus API + `Changed` signal.
crates/tern-cli    `tern` — thin D-Bus control client (status/connect/redeem/import/drives).
crates/tern-gui    gtk4-rs + libadwaita window + ksni tray; D-Bus client.
```

**Backend seam** = `tern_core::backend` traits: `VpnBackend` (static WireGuard config), **`TeleportVpn`**
(invite→session then session→tunnel — separate because Teleport has no dialable endpoint), `MountBackend`,
`Reachability`, `SecretStore`. `StubBackend` implements all of them so the whole engine flow is unit-testable
on any OS with no privilege/bus/account.

**Teleport connection data flow** (all in `tern_core::teleport`):
`Invite::parse` → `Broker::pair` (→ reusable `Session`, persisted in keyring) → `Broker::fetch_ice` →
`ice::local_candidates` + `ice::reflexive_candidate` → `Broker::connect` (offer: our WG pubkey + STUN secret +
candidates) → `nomination::await_nomination` (answer the console's authenticated STUN, track the wait sequence)
→ `dataplane::Tunnel::start` (boringtun over the ICE socket ↔ a `tun-rs` TUN). `teleport::establish()` is the
single function that runs all of this and returns a `Connection { tunnel, client_ip, dns, endpoint }`.

## 5. The Teleport protocol (what we ported, from the Go reference)

Reference: **`sinnet3000/teleport-client`** (Go, MIT) and **`darki73/telepy-cli`** (Python). Broker base:
`https://cloudaccess.svc.ui.com/teleport`.
- **Invite**: `https://teleport.ui.link/<uuid>` (generated in the console: Settings → VPN → Teleport).
  **Single-use** for pairing. `secret_to_token` = base64url(sha512(scrypt(invite, fixed-salt, N=2^14,r=8,p=1,64))).
- **Pair**: `REQUEST_ACCESS` → poll `ACCESS_GRANTED` → a reusable `Session {token, secret, device_token}`.
- **ICE**: `GET_ICE_CONFIGURATION`; gather host + STUN-reflexive candidates (TURN not yet used → off-LAN untested).
- **Connect**: post `CONNECT` (our wg pubkey + a per-connection STUN secret + candidates), poll `CONNECT_RESPONSE`
  (console's wg pubkey, tunnel addr, `client_ip`, `dns_addrs`, `udp_echo_*`, its candidates).
- **Nomination**: the console is **master**; it sends authenticated STUN Binding requests carrying a DATA `wait`
  countdown `[2000,1000,500,250,125]`. We are **slave**: validate MESSAGE-INTEGRITY (HMAC-SHA1 keyed by the STUN
  secret), reply Binding Success, track the sequence per remote tuple; the tuple that completes it is the
  nominated endpoint. **Never originate DATA** — that flips the role and the console won't bring WireGuard up.
- **Data plane**: userspace WireGuard (boringtun) over the *same* ICE socket to the nominated endpoint, bridged
  to a TUN device. Keep answering the console's post-nomination STUN (RFC 7675 consent-freshness) or it stops
  sending WireGuard.

## 6. The addressing / routing model (LIVE-CONFIRMED — this is non-obvious)

The console's `CONNECT_RESPONSE` gives, e.g.: `tunnel_addr = fd37::…:2/120` (**IPv6 ULA overlay**),
`client_ip = 192.168.2.11` (**a v4 address on its LAN**), `dns = [192.168.1.1]`. The ICE-nominated **underlay
endpoint** was a varying LAN IP (192.168.4.1 / .8.1 / .50.1 / .60.1 — the console has many VLANs).
- Assign **both** the v6 overlay and the v4 `client_ip` to `tern0`.
- Route the **remote v4 `/24`s** (derived from `client_ip` + `dns`) via `tern0`, **excluding the underlay
  endpoint's own `/24`** (that's the WireGuard transport path — routing it loops).
- Today this is **split-tunnel** to those derived /24s — it reaches the DNS + client subnets, not arbitrary VLANs.
  Broadening it is the next feature (§9).
- **⚠ ICMP is a red herring here:** `ping` reads 0% loss on this overlapping-internal-network setup — an ICMP
  raw-socket quirk. We proved the decrypted replies *do* arrive on `tern0` (`ip -s link` RX>0, dropped 0) and
  that **real UDP app traffic (a DNS query) works**. Do NOT judge the tunnel by `ping`; use a real service.

## 7. The daemon capability solution (the current open item — READ THIS)

The data plane creates a TUN and runs `ip`/`sysctl`/`resolvectl`. In-process netlink for *addresses* is denied
under SELinux even as root (that's why we **exec `ip`** instead of doing it in-process). The daemon carries
`CAP_NET_ADMIN` as a **file capability** (`setcap cap_net_admin+eip ~/.local/bin/ternd`). Two subtleties that
cost real time:
1. Exec'ing a file-cap binary **clears ambient and leaves inheritable empty** — so execed children get no cap.
   Fix: add `CAP_NET_ADMIN` to the process **inheritable** set (allowed since it's permitted), *then* raise it
   into **ambient**, via the `caps` crate.
2. Ambient caps are **per-thread**. Raising inside `#[tokio::main]` runs *after* worker threads exist, so the
   worker that fork/execs `ip` misses it. Fix: `ternd/src/main.rs` uses a **manual runtime** — raise on the main
   thread **before** `Builder::build()`, **and** in `on_thread_start` on every worker/blocking thread.
   (`/proc/<pid>/status` showing `CapAmb` with bit 12 = `…1000` on the *main* thread was misleading — the worker
   thread was the one that mattered.)

If `tern redeem` reports **"needs permission"/PrivilegeRequired**, that raise didn't take:
`journalctl --user -u ternd… | grep -i cap` shows "raised CAP_NET_ADMIN into inheritable+ambient" (good) or
"not permitted" (the setcap didn't stick — check `getcap ~/.local/bin/ternd`, and note `install` copies a new
inode so setcap must run **after** install). This is the last thing to confirm.

## 8. Rules & licensing (from AGENTS.md — don't relearn the hard way)

- **Commits**: Conventional Commits, lowercase, imperative, no trailing period. **NO attribution trailers**
  (no `Co-Authored-By`, no AI attribution — author as the repo owner). AI use is disclosed once, in the README.
- **Licensing (rule 5, `deny.toml`)**: stay MIT/permissive. The data-plane crates are `boringtun` 0.7 (BSD-3;
  0.6 pins an rc x25519 — don't use it), `tun-rs` (MIT/Apache — **not** the `tun` crate, which is WTFPL),
  `caps` (MIT). `ring` (Apache-2.0 AND ISC) is our existing rustls provider — **not** the banned OpenSSL.
  `BSD-2-Clause` is on the allow-list for `ip_network*` (via boringtun). Run `cargo deny` after any dep change.
- **No secrets in the repo** (rule 6): never commit invites, tokens, session creds, or real console IDs.
- **Keep code + docs in sync in the same commit** (rule 3). There's a test asserting error titles have no jargon
  (rule 4 / docs/05) — keep it passing.

## 9. What's next (needs the owner / live iteration)

1. **Confirm the daemon path** — the TODO.md command. Everything else waits on this.
2. **Broad/full-tunnel routing** — to reach all home VLANs, not just the derived /24s. **First step:** run a
   redeem with `RUST_LOG=debug` and read the logged raw broker response (added for exactly this) — if the
   console advertises routes/subnets, route *those* (authoritative). Else full-tunnel via the wg-quick trick:
   host-route the nominated endpoint via the real gateway, then `0.0.0.0/0` (or the `/1` split / fwmark). **Skip
   any /24 the host is locally on** (multi-homed) so local traffic isn't diverted. Lives in
   `tern-linux::teleport::configure_steps`.
3. **Drives over the tunnel** — `MountBackend` (GVfs) exists; the open question is *discovery* for the
   accountless Teleport case (no UCS API). See "detect shares" in `docs/09` §12; manual-add is the simpler start.
4. **Off-LAN / TURN** — all live tests were on-LAN (direct candidate won). Exercise reflexive/TURN from a remote
   network; the socket is currently IPv4-only (`0.0.0.0:0`).

## 10. Key file pointers

- `crates/tern-core/src/teleport/mod.rs` — `Invite`, `Broker` (pair/fetch_ice/connect, 4xx→InviteAlreadyUsed),
  `establish()` → `Connection`, `Session`.
- `crates/tern-core/src/teleport/dataplane.rs` — `Tunnel` (boringtun pump), `Stats`, consent-freshness reply,
  inbound-packet debug samples. `Tunnel::stop` uses `notify_one` (not `notify_waiters` — that could hang).
- `crates/tern-core/src/teleport/{nomination,stun,ice}.rs` — nomination loop, STUN wire + MESSAGE-INTEGRITY, ICE.
- `crates/tern-core/src/engine.rs` — `redeem_invite`/`connect`/`disconnect`/`sign_out`/`restore_teleport_session`/
  `forget_teleport`; session is set in-memory **before** `up()` so a failed bring-up keeps it (no wasted invite).
- `crates/tern-linux/src/teleport.rs` — `TeleportVpnBackend`, `configure_steps` (the tested iproute2 sequence),
  `run_steps` (EPERM→PrivilegeRequired).
- `crates/ternd/src/main.rs` — `raise_ambient_net_admin()` + manual runtime + `on_thread_start` (see §7).
- `crates/ternd/src/service.rs` — D-Bus methods; slow ops emit `begin_connecting` ("Turning on…") first.
- `DECISIONS.md` ADR-0016 — the full decision + the live-validation + capability notes.
- `docs/02` (protocol), `docs/04` (licensing incl. the data-plane deps table), `docs/09` (GUI design + backlog).

## 11. Things that bit us (so you don't repeat them)

- Stale `ternd`: `install-local.sh` doesn't restart the service; always `systemctl --user restart tern.service`.
- Session file in `/run/user/0` (root's XDG_RUNTIME_DIR under sudo) gets wiped between sudo sessions → the probe
  now saves to `/tmp/tern-teleport-session.json`.
- The old daemon's `redeem_invite` was a placeholder that **validated but did not pair**, so it didn't consume
  the invite; the new one pairs, so a failed run *used* to burn invites — fixed by keeping the session.
- IPv6 on a fresh TUN: clear `net.ipv6.conf.tern0.disable_ipv6=0` **before** assigning the v6 address.
- `ping` 0% ≠ broken tunnel (§6). Always test with a real service (DNS/HTTP).
