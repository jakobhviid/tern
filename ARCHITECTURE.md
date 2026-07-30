# Architecture

How Tern is put together, and why. See `DECISIONS.md` for the reasoning behind each choice and `docs/` for the
research it's based on.

## Shape: a background service + thin clients

```
        ┌──────────────┐        session D-Bus         ┌───────────────┐
        │   tern-gui   │  ── phd.hviid.Tern (JSON) ──  │     ternd     │
        │  window+tray │  ◀── Changed signal ────────  │  (the engine) │
        └──────────────┘                               └──────┬────────┘
        ┌──────────────┐                                      │ trait calls
        │    tern      │  ── same D-Bus interface ──▶         ▼
        │    (CLI)     │                            ┌──────────────────────┐
        └──────────────┘                            │  tern-linux backends │
                                                    │  nmcli · gio · keyring│
                                                    └──────────────────────┘
```

- **`ternd`** owns the session, orchestration, and state; it's the single source of truth. It exposes a tiny
  session-bus interface (`phd.hviid.Tern`) whose methods take/return **JSON strings** and which emits a
  **`Changed`** signal carrying the new snapshot so clients update live (no polling).
- **`tern-gui`** and **`tern`** are thin clients of that interface. The GUI is the autostarted "agent"
  (top-bar tray + optional window); the CLI is for scripting/support.
- All *privileged/system* work is delegated to services that already run privileged/user-space
  (NetworkManager, GVfs, the keyring), so no part of Tern needs root or a custom helper.

## Crates

| Crate | Kind | Responsibility |
|---|---|---|
| `tern-core` | lib | Platform-agnostic: SSO/UCS API client, models, WireGuard keys, the state machine, the error→message taxonomy, config, the **engine**, and the backend **traits**. Fully unit-tested on any OS. |
| `tern-linux` | lib | Linux backend impls of those traits via `nmcli` / `gio` / `secret-tool` (arm's-length subprocesses). |
| `ternd` | bin | The background service: wraps the engine + backends, serves the D-Bus interface. |
| `tern-cli` | bin (`tern`) | Session-bus control client (status/connect/drives; `--json`, `man`, `completions`, `--llm`). |
| `tern-gui` | bin | GTK4 + libadwaita window and `ksni` tray; D-Bus client of `ternd`. |

The seam that matters is `tern-core::backend` — four traits (`VpnBackend`, `MountBackend`, `Reachability`,
`SecretStore`). `tern-core` provides an in-memory `StubBackend` (so the whole flow runs on macOS/CI with a
mock UCS server); `tern-linux` provides the real ones. Swapping a backend (e.g. the planned D-Bus VPN backend
for Flatpak, ADR-0014) touches no core code.

## The core flow (engine)

1. **Sign in** — browser SSO (passkey-capable) yields a bearer token → `engine.sign_in` fetches identity + hosts.
2. **Connect** — ensure a device WireGuard keypair (keyring), enroll the public key, `POST vpn/session` to get a
   WireGuard config, bring the tunnel up via the VPN backend, then **auto-mount the selected, reachable drives**.
3. **Snapshot** — a consistent view (`Auth` / `Access` / per-drive state) that every client renders. User-facing
   strings only; never protocol/impl jargon (there's a test enforcing this).

## Threads in the GUI

Three threads, bridged by `async-channel` (runtime-agnostic), so tokio and glib never mix:
- **GTK main** — renders the snapshot, owns the widgets.
- **tokio actor** — holds the D-Bus proxy, runs commands, forwards the `Changed` signal.
- **ksni tray** — its own thread (ksni blocking API); menu actions route through the same channels.

## What's real vs. pending

Built + tested/compiled on macOS: all five crates, the engine end-to-end (mock UCS + stub), the D-Bus service,
the CLI, the GTK window, and the tray. Pending (needs the Linux box / a real account): the browser+loopback SSO
flow (ADR-0009), runtime validation of the nmcli/gio/keyring backends, the D-Bus VPN backend for Flatpak
(ADR-0014), and confirming the UCS request/response shapes by traffic capture (M7). See `docs/06-build-plan.md`.
