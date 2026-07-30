#!/usr/bin/env bash
# Dev install of Tern from source into ~/.local — for testing on Linux (e.g. Bazzite).
# Builds release binaries and installs the desktop/icon/metainfo/systemd/D-Bus files for the current user.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> Building release binaries…"
cargo build --release --bin ternd --bin tern --bin tern-gui

BIN="$HOME/.local/bin"
mkdir -p "$BIN"
install -m755 target/release/ternd    "$BIN/ternd"
install -m755 target/release/tern     "$BIN/tern"
install -m755 target/release/tern-gui "$BIN/tern-gui"

install -Dm644 packaging/phd.hviid.Tern.desktop \
  "$HOME/.local/share/applications/phd.hviid.Tern.desktop"
install -Dm644 data/icons/hicolor/scalable/apps/phd.hviid.Tern.svg \
  "$HOME/.local/share/icons/hicolor/scalable/apps/phd.hviid.Tern.svg"
install -Dm644 packaging/phd.hviid.Tern.metainfo.xml \
  "$HOME/.local/share/metainfo/phd.hviid.Tern.metainfo.xml"
install -Dm644 packaging/systemd/tern.service \
  "$HOME/.config/systemd/user/tern.service"

# D-Bus session activation pointing at the user-installed binary. The daemon owns a sub-name of the
# desktop app-id (phd.hviid.Tern) so it never collides with the GUI's GtkApplication (see ADR-0015).
mkdir -p "$HOME/.local/share/dbus-1/services"
cat > "$HOME/.local/share/dbus-1/services/phd.hviid.Tern.Daemon.service" <<EOF
[D-BUS Service]
Name=phd.hviid.Tern.Daemon
Exec=$BIN/ternd
SystemdService=tern.service
EOF

command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "$HOME/.local/share/applications" || true
command -v gtk4-update-icon-cache >/dev/null 2>&1 \
  && gtk4-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
systemctl --user daemon-reload || true

cat <<'EOF'

Installed to ~/.local. Next:
  systemctl --user enable --now tern.service   # start the background service
  tern status                                  # should print "Not signed in"
  tern-gui                                      # window + top-bar tray

GNOME needs the "AppIndicator and KStatusNotifierItem Support" extension for the tray icon.
Make sure ~/.local/bin is on your PATH.
EOF
