# Build Plan & Status

Milestone plan and a running status board. Updated as work lands. "Testable where" matters because this is
being built on macOS but targets Linux/GNOME (Bazzite) — some layers can be proven here, some only there.

## Milestones

| # | Milestone | State | Testable where |
|---|---|---|---|
| M0 | Research: teardown, protocol, licensing, UX (docs/) | ✅ done | — |
| M1 | `tern-core`: models, errors→UX, state machine, wg keys, UCS client, backends, **engine** | ✅ done | macOS/CI (mock UCS + stub) |
| M2 | `tern-cli`: control client, clap (json/man/completions/llm) | ✅ done | macOS (help/man/completions/llm); Linux (IPC) |
| M3 | `ternd`: session-bus D-Bus service wrapping the engine (+ Changed signal) | ✅ built | builds+clippy on macOS; runs on Linux |
| M4 | `tern-linux`: NetworkManager/GVfs/keyring backends via CLIs (nmcli/gio/secret-tool) | ✅ built | compiles on macOS; runs on Bazzite. D-Bus port needed for Flatpak (ADR-0014) |
| M5 | `tern-gui`: GTK4/libadwaita window + live D-Bus updates | 🟡 window built | **compiled+linked against real gtk4 on macOS**; `ksni` tray pending (Bazzite) |
| M6 | Packaging: systemd unit, D-Bus activation, desktop, AppStream, icon, Flatpak manifest | 🟡 scaffolded | offline cargo-sources + release CI + tap template pending (Linux/CI) |
| M7 | Real-account validation: traffic capture → confirm UCS shapes; browser+loopback auth | ⏳ | Bazzite + owner creds |

### Remaining before "usable on Bazzite"
- **ksni tray** (M5) — the top-bar icon; needs a real SNI host to verify.
- **Browser+loopback SSO** (ADR-0009) — replace the `CompleteSignIn(token)` placeholder with the real flow
  (passkey-capable). Exact OAuth params need the M7 capture.
- **Runtime bring-up on Bazzite** — build from source, `systemctl --user enable --now tern.service`, run the GUI,
  and iterate the nmcli/gio/secret-tool backends against the real system.

## What's proven on the Mac right now (M1)

`cargo test -p tern-core` → 23 tests green (incl. wiremock HTTP round-trips + full engine flow).
`cargo run -p tern-core --example flow` → runs sign-in → provision → connect → **selective auto-mount** →
disconnect, and prints the plain-language UX + error rendering. No display, D-Bus, or UniFi account needed.

## Known unknowns to confirm on Bazzite (with a traffic capture)

1. **UCS request/response shapes** — paths + field names are from binary recon (HIGH); exact JSON bodies of
   `identity/public_key` and `vpn/session` are MEDIUM. Confirm and tighten `model.rs` / `ucs.rs`.
2. **Drive-list endpoint** — `ucs::UcsClient::drives()` path is a best guess (marked UNCONFIRMED); the engine
   treats failure as "no drives", so a correction is low-risk.
3. **NetworkManager user-owned WireGuard** password-free toggle on Fedora (ADR-0004).
4. **GVfs SMB** reachability + keyring persistence reliability on Bazzite (ADR-0005).
5. **Tray icon** appearing on Bazzite's GNOME (needs the AppIndicator extension — ADR-0006/doc 03).
6. **SSO callback** (`x-scheme-handler/identity-standard`) round-trip, incl. inside Flatpak (ADR-0009).

## Ordering rationale

Core first (M1) because it's the risky, logic-heavy part *and* the only part fully testable on macOS — so it's
proven before touching Linux-only integration. CLI next (M2) to have a driver. Then the daemon shell (M3),
then the Linux backends (M4) and GUI (M5) which need the real OS. Packaging (M6) once there's something to
ship. Live-account validation (M7) is deliberately last and gated on the owner (needs real credentials, which
never enter this repo).
