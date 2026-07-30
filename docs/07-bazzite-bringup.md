# Bazzite bring-up — build, install, run, iterate

Practical steps to get Tern running on the Bazzite (Fedora Atomic / GNOME) test box and start iterating the
Linux-only parts. Everything below was built + compiled on macOS; this is where it gets *run*.

## 0. Build environment (immutable OS)

Bazzite is rpm-ostree — don't layer `-devel` packages. Two good options:

- **Distrobox (recommended for building):**
  ```sh
  distrobox create --name tern-dev --image fedora:41
  distrobox enter tern-dev
  sudo dnf install -y cargo rust gtk4-devel libadwaita-devel glib2-devel
  ```
- **Homebrew (matches your bazzite-custom setup):**
  ```sh
  brew install rust gtk4 libadwaita
  export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:$(brew --prefix)/share/pkgconfig"
  ```

Runtime deps (already on Bazzite GNOME, verify): `NetworkManager` (`nmcli`), `gvfs` + the SMB backend
(`gvfs-smb`), `libsecret` (`secret-tool`), `gnome-keyring`.

## 1. Build + install (from source, into ~/.local)

```sh
git clone https://github.com/jakobhviid/tern && cd tern
./packaging/install-local.sh
```

That builds release binaries, drops `ternd`/`tern`/`tern-gui` in `~/.local/bin`, installs the desktop file,
icon, metainfo, a systemd `--user` unit, and a D-Bus activation file. (Ensure `~/.local/bin` is on `PATH`.)

## 2. Start it

```sh
systemctl --user enable --now tern.service   # background service (or: RUST_LOG=info ternd)
tern status                                  # → "Not signed in"
tern-gui                                      # window + top-bar tray
```

**Tray on GNOME:** install the *AppIndicator and KStatusNotifierItem Support* extension (Extension Manager or
extensions.gnome.org) — GNOME has no built-in tray. KDE Plasma shows it natively.

## 3. First smoke test (no account needed)

This validates the whole daemon ↔ CLI ↔ GUI ↔ tray plumbing on Linux before any real auth:
- `tern status` and the GUI both show **Not signed in**; the tray icon appears and its menu works.
- `journalctl --user -u tern -f` (or `RUST_LOG=debug`) shows the service logs.
- The `Changed` signal → GUI/tray update live when state changes.

## 4. Wiring real auth + VPN (M7)

The browser+loopback SSO flow (ADR-0009) isn't built yet — `ternd` exposes a `CompleteSignIn(token)` placeholder.
Two paths to make a real connection work:

1. **Confirm the protocol by capture** (recommended first): run `mitmproxy`, trust its cert, launch the macOS
   *UniFi Endpoint* app, and watch the flow — `sso.ui.com` login, then `api-gw.uid.df.ui.com` calls
   (`/proxy/users/public/api/v2/identity/*`, `/proxy/ucs/public/user/api/v1/vpn/session`). Compare the exact
   request/response JSON to `docs/02` and tighten `tern-core::{model,ucs}` accordingly.
2. **Implement the loopback SSO** in `tern-core` (browser-open + `127.0.0.1` listener + token exchange), then
   `ternd` stores the token and the existing engine flow takes over.

Once a real `vpn/session` config is returned, `tern-linux::nm` writes a wg-quick file and does
`nmcli connection import type wireguard …` → `up`. Watch `nmcli connection show --active` and the app logs.
Drives then auto-mount via `gio mount smb://…` for the ones you ticked.

⚠️ Bringing the VPN up changes routing on this box — expected; just know it's the real thing.

## 5. Likely first iterations on Bazzite

- **nmcli import**: confirm the wg-quick rendering imports cleanly; adjust field formatting if NM is picky.
- **user-owned toggle**: verify `connection.permissions user:$USER` gives password-free up/down.
- **GVfs SMB**: confirm `gio mount` works for your UNAS shares; wire file-service credentials from the keyring
  for authenticated shares (currently non-interactive).
- **Reachability**: replace the WAN-probe stub with a real LAN-vs-VPN + drive-host check.
- **Tray**: confirm the icon + menu on your GNOME setup; tune the icon names if the theme lacks them.
- **Flatpak** (later): port the VPN backend to D-Bus/NetworkManager (ADR-0014), generate `cargo-sources.json`,
  then `flatpak-builder` the manifest in `packaging/flatpak/`.
