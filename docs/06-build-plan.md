# Build Plan & Status

Milestone plan and a running status board. Updated as work lands. "Testable where" matters because this is
being built on macOS but targets Linux/GNOME (Bazzite) — some layers can be proven here, some only there.

## Milestones

| # | Milestone | State | Testable where |
|---|---|---|---|
| M0 | Research: teardown, protocol, licensing, UX (docs/) | ✅ done | — |
| M1 | `tern-core`: models, errors→UX, state machine, wg keys, UCS client, backends, **engine** | ✅ done | macOS/CI (mock UCS + stub) |
| M2 | `tern-cli`: control client + offline render; clap (json/man/completions/llm) | ⏳ next | macOS (render); Linux (daemon IPC) |
| M3 | `ternd`: background service + D-Bus API; session persistence, auto-reconnect | ⏳ | builds macOS; runs Linux |
| M4 | `tern-linux`: NetworkManager (zbus), GVfs mount, libsecret backends | ⏳ | Bazzite only |
| M5 | `tern-gui`: relm4 + libadwaita panels + ksni tray | ⏳ | GUI: macOS via brew gtk4; tray: Linux |
| M6 | Packaging: Flatpak manifest (primary), rpm/deb, CLI tap bottle; release CI | ⏳ | Linux/CI |
| M7 | Real-account validation: traffic capture confirms UCS shapes; wire live auth | ⏳ | Bazzite + owner creds |

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
