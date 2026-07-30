# Workflows

Common tasks in this repo. See `AGENTS.md` for the rules and `ARCHITECTURE.md` for how it fits together.

## Build & test (the gate)

```sh
cargo clippy --workspace --all-targets -- -D warnings   # the release gate (warnings = errors)
cargo test --workspace                                   # no fmt gate by design
```

On macOS the GUI crate needs GTK to build:
```sh
brew install gtk4 libadwaita
export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:$(brew --prefix)/share/pkgconfig"
```

## See the core flow run (no daemon, no account)

```sh
cargo run -p tern-core --example flow   # sign-in → connect → selective auto-mount, against a mock UCS + stub
```

## Run on Linux (Bazzite)

```sh
./packaging/install-local.sh                 # build + install ternd/tern/tern-gui + unit/desktop/icon into ~/.local
systemctl --user enable --now tern.service   # start the background service
tern status                                  # or: tern login / tern connect / tern drives
tern-gui                                      # window + top-bar tray
```
Full detail + first-run + auth capture: `docs/07-bazzite-bringup.md`.

## Connect via Teleport (the real consumer-account path, ADR-0016)

Generate an invite in the console (Settings → VPN → Teleport) — `https://teleport.ui.link/<uuid>` — then:

```sh
sudo setcap cap_net_admin+ep ~/.local/bin/ternd   # once; the in-process TUN needs it (install-local.sh does this)
systemctl --user restart tern.service
tern redeem https://teleport.ui.link/<uuid>       # pairs, persists the session, brings the tunnel up
tern status                                        # Access: On
tern disconnect      # later re-connect with `tern connect` — the saved session needs no new invite
```

The invite is **single-use**; the reusable session is stored in the keyring, so the Access toggle (CLI/GUI/tray)
reconnects without it. **Bench the data plane directly** (no daemon) with the live probe — it prints the
handshake/echo/byte stats and self-terminates:

```sh
cargo build -p tern-core --example teleport_tunnel_probe
sudo ./target/debug/examples/teleport_tunnel_probe <teleport.ui.link invite | saved-session.json>
```

Still open (want a live run to settle): full-tunnel/LAN **routing** and **DNS** (only the connected subnet is
added today), and SMB **drives** over the tunnel. See `TODO.md` stage ⑥–⑦.

## Commits & releases

- **Conventional Commits**, lowercase, imperative; `feat`/`fix`/`docs`/`chore`/`refactor`/`test`/`ci`/`perf`,
  `feat!` for breaking. **No attribution trailers** (see `AGENTS.md`). Version is derived from history.
- Trunk-based: push to `main`; CI (`.github/workflows/ci.yml`) runs clippy + test.
- **Releasing** (future): Flatpak → Flathub (`packaging/flatpak/`), the `tern` CLI → the owner's Homebrew tap.
  Neither is wired yet; the tap is gated per owner instruction.

## Track the macOS app for drift

```sh
scripts/fingerprint-macos-app.sh > "docs/fingerprints/$(date +%F)-vX.Y.Z.txt"
diff docs/fingerprints/<prev>.txt docs/fingerprints/<new>.txt   # spot endpoint/entitlement/feature changes
```
See `docs/00-drift-tracking.md`.

## Add or change a backend

Implement the relevant trait from `tern_core::backend` (`VpnBackend` / `MountBackend` / `Reachability` /
`SecretStore`) in `tern-linux` (or a new crate) and select it in `ternd::build_engine`. No `tern-core` changes.
Example: the planned **D-Bus NetworkManager** VPN backend for Flatpak (ADR-0014) replaces the `nmcli` one.
