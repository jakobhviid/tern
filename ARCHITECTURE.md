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
| `tern-core` | lib | Platform-agnostic: SSO/UCS API client, models, WireGuard keys, the state machine, the error→message taxonomy, config, the **engine**, the backend **traits**, and the **Teleport client** (invite → broker pairing → ICE/STUN nomination → userspace-WireGuard/TUN data plane, ADR-0016). Fully unit-tested on any OS. |
| `tern-linux` | lib | Linux backend impls of those traits via `nmcli` / `gio` / `secret-tool` (arm's-length subprocesses), **plus the Teleport data-plane backend** (in-process TUN + iproute2; needs `CAP_NET_ADMIN`). |
| `ternd` | bin | The background service: wraps the engine + backends, serves the D-Bus interface. |
| `tern-cli` | bin (`tern`) | Session-bus control client (status/connect/drives; `--json`, `man`, `completions`, `--llm`). |
| `tern-gui` | bin | GTK4 + libadwaita window and `ksni` tray; D-Bus client of `ternd`. |

The seam that matters is `tern-core::backend` — five traits (`VpnBackend`, `TeleportVpn`, `MountBackend`,
`Reachability`, `SecretStore`). `VpnBackend` takes a static WireGuard config; `TeleportVpn` is separate because
Teleport has no dialable endpoint — its config is built live from the console, so the flow is invite → session
then session → tunnel (ADR-0016). `tern-core` provides an in-memory `StubBackend` (so the whole flow runs on
macOS/CI with a mock UCS server); `tern-linux` provides the real ones. Swapping a backend (e.g. the planned
D-Bus VPN backend for Flatpak, ADR-0014) touches no core code.

## The core flow (engine)

Two ways to get connected, one Access state out:

1. **Get connected** — either:
   - **Teleport** (the consumer path, ADR-0016): `engine.redeem_invite` pairs a `teleport.ui.link` invite into a
     reusable session (persisted to the keyring), then the `TeleportVpn` backend runs the data plane — ICE
     candidate exchange → `CONNECT` → answer the console's nomination → boringtun over the ICE socket into a TUN.
     The Access toggle reconnects the stored session with no invite.
   - **Account**: browser SSO yields a bearer token → `engine.sign_in`; `engine.connect` ensures a device keypair
     (keyring), enrolls the public key, `POST vpn/session` for a WireGuard config, up via `VpnBackend`. (An
     imported plain `.conf` is a third, sign-in-free way in.)
2. **Auto-mount** the selected, reachable drives once Access is on.
3. **Snapshot** — a consistent view (`Auth` / `Access` / per-drive state) that every client renders; a Teleport
   or imported tunnel shows as connected even while signed out. User-facing strings only; never protocol/impl
   jargon (there's a test enforcing this).

## Threads in the GUI

Three threads, bridged by `async-channel` (runtime-agnostic), so tokio and glib never mix:
- **GTK main** — renders the snapshot, owns the widgets.
- **tokio actor** — holds the D-Bus proxy, runs commands, forwards the `Changed` signal.
- **ksni tray** — its own thread (ksni blocking API); menu actions route through the same channels.

## What's real vs. pending

Built + tested: all five crates, the engine end-to-end (mock UCS + stub), the D-Bus service, the CLI, the GTK
window, and the tray. The **Teleport control plane is validated live** against a real console (invite → session
→ ICE → connect → nomination), and the userspace-WireGuard/TUN **data plane is built + gated** (boringtun
handshake round-trip test; iproute2 setup unit-tested). Pending: the **live data-plane run** (WireGuard
handshake + return traffic through the TUN, then routing/DNS) — needs `CAP_NET_ADMIN` on the box; then drives
over the tunnel, and the D-Bus VPN backend for Flatpak (ADR-0014). See `TODO.md` and `docs/06-build-plan.md`.
