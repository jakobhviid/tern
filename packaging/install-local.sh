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

# The Teleport data plane creates a TUN device in-process (ADR-0016), which needs CAP_NET_ADMIN. A systemd
# --user service can't be granted ambient capabilities, so we set a file capability on the binary instead.
# One-time, needs root; the tunnel simply won't come up without it (the daemon reports "privilege required").
if command -v setcap >/dev/null 2>&1; then
  echo "==> Granting CAP_NET_ADMIN to ternd (for the Teleport TUN; needs sudo)…"
  sudo setcap cap_net_admin+ep "$BIN/ternd" \
    || echo "   (skipped — run 'sudo setcap cap_net_admin+ep $BIN/ternd' yourself to enable the tunnel)"
else
  echo "==> setcap not found — run 'sudo setcap cap_net_admin+ep $BIN/ternd' to enable the Teleport tunnel."
fi

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
  tern connect --invite <teleport.ui.link/…>   # redeem a Teleport invite and bring up the tunnel

If the tunnel reports "privilege required", grant the capability the Teleport TUN needs:
  sudo setcap cap_net_admin+ep ~/.local/bin/ternd  &&  systemctl --user restart tern.service

GNOME needs the "AppIndicator and KStatusNotifierItem Support" extension for the tray icon.
Make sure ~/.local/bin is on your PATH.
EOF
