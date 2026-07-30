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
