# UniFi Endpoint → Linux

Research + design notes toward a **GNOME-native Linux client** that mirrors Ubiquiti's macOS
**UniFi Endpoint** (UniFi Identity / UID) desktop client, plus tooling to **track the Mac app over time**
so we can keep the port in sync and fix it when Ubiquiti changes something.

## Goal

A GNOME-first (KDE later) menu-bar-style app, Rust preferred, that can:
1. **Log in** via UniFi SSO (browser OAuth).
2. **Connect the One-Click VPN** (WireGuard / "Teleport-style").
3. **Link to the admin/manage site.**
4. **Auto-mount UniFi Drive shares** — selectively, per-drive, whenever reachable (LAN or over the VPN).

Distribution targets: **Flatpak** (primary, if feasible) + native `.deb`/`.rpm`/AUR; Homebrew evaluated.

## Scope decisions

- **In scope:** the four capabilities above, with per-drive selective auto-mount that works on the LAN and
  over the One-Click VPN.
- **Out of scope:** roaming **remote drive access without a VPN**. The Mac app does this via UniFi's
  proprietary **CloudAccess WebRTC relay** (bridged through the UNAS/console web plane) — not replicable by a
  third party without reverse-engineering UI's relay + auth. See docs 01/02.

## What we know so far (headlines)

- The Mac app is the **UniFi Identity (UID) client** (`com.ui.uid.standard-desktop`), a menu-bar agent.
- **VPN engine:** userspace **WireGuard** via **sing-box + sagernet/wireguard-go + gVisor netstack**, run as
  a macOS Network-Extension system extension. Config is **cloud-provisioned per session — not exportable**;
  provisioned by a clean REST **"UCS" API** (`POST .../vpn/session` → a standard WireGuard config), *not* the
  consumer-Teleport ICE/STUN broker (confirmed from the binary — see doc 02).
- **Drives:** SMB via a `NetFSMountCoordinator`; a separate CloudAccess/WebRTC path for remote (no-VPN) access.
- **No official Linux client exists**, but **two open-source reverse-engineered clients do**
  (`darki73/telepy-cli`, `sinnet3000/teleport-client`) and document the auth/broker handshake.
- **Feasibility:** medium (~9–13 weeks v1) using a **delegate-to-NetworkManager + GVfs** design that keeps the
  app unprivileged and makes a real Flatpak viable.

## Docs

| Doc | What's in it |
|---|---|
| [`docs/00-drift-tracking.md`](docs/00-drift-tracking.md) | How we fingerprint the Mac app + diff releases; version log |
| [`docs/01-macos-endpoint-teardown.md`](docs/01-macos-endpoint-teardown.md) | First-hand teardown of the installed Mac app (v4.1.1) |
| [`docs/02-vpn-protocol-and-reference-clients.md`](docs/02-vpn-protocol-and-reference-clients.md) | One-Click VPN provisioning/auth flow + reusable reference clients |
| [`docs/03-linux-feasibility-and-architecture.md`](docs/03-linux-feasibility-and-architecture.md) | GNOME-native architecture, recommended Rust stack, effort estimate |
| [`docs/04-dependencies-and-licensing.md`](docs/04-dependencies-and-licensing.md) | Every dependency, its license + link, GPL-contagion rules, packaging status |
| [`docs/05-ux-and-error-handling-guidelines.md`](docs/05-ux-and-error-handling-guidelines.md) | Consumer-desktop UX: state model, plain-language errors, notifications |
| [`docs/06-build-plan.md`](docs/06-build-plan.md) | Milestones + status board; what's proven where |
| [`docs/07-bazzite-bringup.md`](docs/07-bazzite-bringup.md) | Build/install/run/iterate on the Bazzite (Fedora Atomic) test box |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | How the crates + daemon/clients fit together |
| [`DECISIONS.md`](DECISIONS.md) | Append-only decision log (ADR-####) with options weighed |
| [`AGENTS.md`](AGENTS.md) | Working guide + rules for humans/agents in this repo |
| [`WORKFLOWS.md`](WORKFLOWS.md) | Common tasks: build/test, run, install on Linux, drift-check |
| [`docs/fingerprints/`](docs/fingerprints/) | Raw dated fingerprints of each Mac app version |

## Tooling

- [`scripts/fingerprint-macos-app.sh`](scripts/fingerprint-macos-app.sh) — read-only probe of
  `/Applications/UniFi Endpoint.app`; emits a diffable summary. Re-run after each app update (see doc 00).

## Status

Working vertical slice, built and compiled end-to-end (much of it on macOS, all of it green on Ubuntu CI):

- **`tern-core`** — auth/UCS client, WireGuard keygen, state machine, backend traits, and the orchestration
  **engine**; 24 tests incl. mock-HTTP round-trips and the full flow.
- **`ternd`** — background service exposing the engine over a session-bus D-Bus API (+ live `Changed` signal).
- **`tern-linux`** — NetworkManager/GVfs/keyring backends (via `nmcli`/`gio`/`secret-tool`).
- **`tern`** (CLI) and **`tern-gui`** (GTK4 + libadwaita window **+ top-bar tray**) — thin D-Bus clients.
- **Packaging** — systemd unit, D-Bus activation, desktop entry, AppStream metainfo, icon, Flatpak manifest,
  and `packaging/install-local.sh`.

Next (needs the Linux box / a real account): the browser+loopback SSO flow, runtime validation of the
backends, and confirming the UCS wire shapes by capture — see [`docs/06-build-plan.md`](docs/06-build-plan.md)
and [`docs/07-bazzite-bringup.md`](docs/07-bazzite-bringup.md). Baseline macOS-app fingerprint: **v4.1.1
(build 177)**, 2026-07-30.

```sh
cargo test -p tern-core                 # 24 tests, incl. HTTP round-trips + full engine flow
cargo run -p tern-core --example flow   # watch sign-in → connect → selective auto-mount execute
./packaging/install-local.sh            # (on Linux) build + install ternd/tern/tern-gui into ~/.local
```

## AI disclosure

Parts of this codebase and its research were written with the assistance of AI coding agents (Claude Code and
others). All changes are reviewed by the maintainer.

## Note on legality

This is a clean-room interoperability effort against a service the user is authorized to use. We reference
the open-source clients for protocol understanding; any code reuse depends on their licenses (see doc 04).
Nothing here circumvents authentication — it reproduces a login the user is entitled to perform.
