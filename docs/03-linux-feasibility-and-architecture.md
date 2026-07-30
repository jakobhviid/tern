# Linux (GNOME-first) Client — Feasibility & Architecture

> Synthesis of implementation research (GNOME 46–48 / Ubuntu 24.04–25.04 / Fedora 40–42 / Plasma 6),
> mapped onto the four capabilities from the macOS teardown (doc 01). Confidence tags inline.

## Verdict up front

**Feasible, medium effort.** The hard part is not the Linux desktop plumbing (that's well-trodden) — it's
**reproducing UniFi's cloud auth + VPN-provisioning handshake** (doc 02), because the One-Click VPN config
can't be exported. **Update:** that handshake is now mapped from the binary — it's a clean REST flow (SSO →
upload device public key → `POST ucs/.../vpn/session` returns a standard WireGuard config), *not* the
consumer-Teleport ICE/STUN broker, so the VPN piece is more tractable than first feared. Everything else
(tray, GUI, WireGuard bring-up, SMB mount, SSO deep-link) has clean,
modern, mostly-Rust building blocks. Rough estimate **≈ 9–13 weeks** to a solid v1 for one experienced
Rust desktop dev, *if* we delegate privileged work to NetworkManager + GVfs (see below). A custom
privileged VPN helper adds **+3–5 weeks** and costs us the pure-Flatpak option.

## The one design decision that determines everything

**Delegate all privileged work to services that already run privileged.** Concretely:
- **VPN → NetworkManager** (runs as root, has native WireGuard since NM 1.16, exposes a polkit-gated D-Bus API).
- **SMB → GVfs** (`gio mount`, a per-user userspace daemon, no root).

Do this and the app itself never needs `CAP_NET_ADMIN` or a root helper — which simultaneously gives us
**least privilege** and the **cleanest distribution story** (a real Flatpak becomes viable). The alternative
(own root helper) is only justified for a bespoke kill-switch / boot-time blocking / userspace tunnel.

## Capability-by-capability mapping

### 1. Login / SSO  → browser OAuth + custom URL scheme
- Register `x-scheme-handler/identity-standard` via a `.desktop` `MimeType=` + `xdg-mime default`. Browser
  hits `identity-standard://…`, launches (or activates) the app. Use **GApplication single-instance**;
  verify `HANDLES_OPEN` vs `HANDLES_COMMAND_LINE` delivery on the target GLib (bare custom schemes aren't
  always delivered as a `GFile`). **[high; delivery nuance medium]**
- Implement the actual token flow from doc 02 (`sso.ui.com` login+MFA → session → SigV4 to
  `cloudaccess.svc.ui.com`). Store tokens in the keyring via **`oo7`** (portal-aware; best under Flatpak).
- Flatpak caveat: test the full OAuth round-trip in-sandbox (scheme registration works but has rough edges).

### 2. One-Click VPN  → NetworkManager native WireGuard over D-Bus
- WireGuard is **kernel-native since Linux 5.6**; `wg`/`wg-quick` need root/`CAP_NET_ADMIN`. Instead drive
  **NetworkManager** via **`zbus`**. **[high]**
- **Least-privilege trick:** store the tunnel as a **user-owned connection** (`connection.permissions=user:$USER`).
  Per NM's polkit defaults, `network-control` (activate/deactivate) and `settings.modify.own` are
  `allow_active = yes` → **password-free toggling for the logged-in user**; only system-wide profiles need
  admin. Verify `AddConnection→modify.own` gating on the target NM version. **[high / medium]**
- **Split tunnel** = per-peer `allowed-ips` (list only LAN subnets); full tunnel = `0.0.0.0/0, ::/0`
  (NM/wg-quick handle the fwmark + policy-routing so encrypted packets bypass the tunnel). Mirrors the Mac
  app's "Split Tunneling" / "Traffic Sent Through VPN / Bypassed" UI. **[high]**
- No official NM Rust binding — codegen proxies from NM's introspection XML with `zbus-xmlgen`. Direct-mgmt
  crates exist (`defguard_wireguard_rs`, `wireguard-control`) but they need privileges → avoid.

### 3. Auto-mount drives  → GVfs `smb://` (default) or kernel `mount.cifs` (if CLI apps need it)
- **Networking precondition (confirmed):** SMB is TCP/445 with **no NAT traversal** — the tunnel must be up
  first. Same constraint for every mount method. (This is *why* the Mac app's remote-without-VPN path needs
  UI's proprietary CloudAccess relay — out of scope, see doc 01/02.) **[high]**
- **Default: `gio mount smb://…` (GVfs)** — userspace, no root, native Nautilus integration, keyring creds.
  Downside: non-GIO/CLI apps only reach it via the lossy `gvfsd-fuse` bridge at `/run/user/UID/gvfs`.
- **If arbitrary/CLI apps must see the files: kernel `mount.cifs` via a systemd `.automount` unit**, ordered
  **after the VPN** (not just `network-online.target`), creds from a root-only file populated from the keyring,
  `uid=`/`gid=` = desktop user.
- **Selective per-drive auto-mount** (the user's ask): list the user's UniFi Drive shares (from the UID API),
  let the user tick which to auto-mount, persist per profile, and mount each **when its target is reachable**
  (LAN or VPN-up) — auto-unmount when it goes away. Mirror the Mac app's "Local Network Connected / Not
  Mounted" reachability states, minus the cloud-relay fallback. Trigger on `vpn-up`/`vpn-down`
  (NetworkManager-dispatcher) or right after the app sees NM report the VPN active.
- Note: the Mac app also does **UniFi Drive on-demand placeholder files** ("show up in Finder without taking
  up hard drive space") — that's a File-Provider/sync feature beyond plain SMB; treat as a later nice-to-have.

### 4. Admin / manage links  → just open URLs
- Open `unifi.ui.com` / `account.ui.com/manage` / Drive Portal in the default browser (`xdg-open` / `ashpd`
  OpenURI). Trivial. **[high]**

## Top-bar icon (the GNOME wrinkle)

**GNOME has no built-in tray** (removed in 3.26, never replaced). A persistent status icon **always** needs
an SNI/AppIndicator host:
- **Ubuntu default session:** works out of the box (ships + enables `ubuntu-appindicators`). **[high]**
- **Fedora / vanilla GNOME:** user must install the **AppIndicator extension (EGO #615)**. **[high]**
- **KDE Plasma:** native, no extension. **[high]**

Options: (i) ship with **`ksni`** (pure-Rust StatusNotifierItem) and document the one-time Fedora extension
install; (ii) bundle our own minimal GJS Shell extension (ongoing per-Shell-version upkeep); or (iii) go
GNOME-idiomatic — a normal background app (Background portal) surfacing state via **notifications** + a
Quick Settings-like window, instead of a tray. **Recommendation:** `ksni` + document the extension; keep a
windowed fallback so the app is fully usable even where the icon can't render.

## Recommended stack (Rust)

| Concern | Choice | Why |
|---|---|---|
| GUI | **relm4 + gtk4-rs 0.11 + libadwaita 0.9** | Only stack that yields a true Adwaita/GNOME-HIG look; fastest for small MVU panels. Raw gtk4-rs is a fine alt. |
| Tray | **`ksni`** | Pure-Rust SNI, no libappindicator C dep. |
| VPN control | **`zbus`** → NetworkManager | No privilege in our process; password-free user toggling. |
| SMB | **`gio` / GVfs** (default), kernel `mount.cifs` (opt) | "Just works" in Files; no root. |
| Secrets | **`oo7`** (libsecret/portal) | Keyring, Flatpak-aware. |
| Notifications | **`notify-rust`** (or `ashpd` for portal) | Standard. |
| Deep link | `.desktop` `x-scheme-handler` + GApplication | SSO callback. |
| Autostart | systemd **user** service (or autostart `.desktop`; Background portal in Flatpak) | "Connect at startup" parity. |

Alternatives considered: **Tauri** (web UI — not Adwaita-native, WebKitGTK version-skew), **Iced/libcosmic**
(COSMIC look, not GNOME), **Slint** (own renderer + tri-license gotcha). All rejected for a GNOME-native goal.

## Distribution (short version — full license/packaging detail in doc 04)

- **Flatpak (Flathub) — viable *with the delegated design*.** Sandbox can never get `CAP_NET_ADMIN` or install
  a daemon, but it can `--system-talk-name=org.freedesktop.NetworkManager` (VPN) and `--talk-name=org.gtk.vfs.*`
  + `--filesystem=xdg-run/gvfs` (SMB). This is exactly how GNOME Settings' Flatpak manages NM. **[high/medium]**
- **Native `.deb`/`.rpm`/AUR** — ship in parallel for non-NM / enterprise hosts, and **required** if we ever add
  a privileged systemd helper (native packaging can enable it at install; Flatpak cannot). **[high]**
- **Homebrew on Linux — not viable** for this app: single-user, refuses sudo, manages no system services, and
  Linux casks are binary-CLI-only (no GUI bundles). At most a CLI component. **[high]**

## De-risk first (spikes, ~days each)
1. **OAuth deep-link round-trip end-to-end**, including inside Flatpak.
2. **Confirm the UCS `vpn/session` JSON shapes** — the endpoints + config fields are now known from the binary
   (doc 02 UPDATE); one traffic capture confirms exact request/response. Downgraded from "#1 unknown" to a
   quick confirmation.
3. **NM user-owned WireGuard connection** giving genuinely password-free toggling on Ubuntu + Fedora.
4. **Tray icon actually appearing** on the target GNOME setups (Fedora needs the extension).
