# TODO — continuing from a Linux desktop

Entry point for whoever (human or LLM) picks this up **on a Linux/GNOME machine** (e.g. Bazzite). It's the
prioritized work queue + the context you need. For *how to build/install/run* the mechanics, see
[`docs/07-bazzite-bringup.md`](docs/07-bazzite-bringup.md) (this file links to it rather than repeating it).

## Read first (5 min)
1. [`AGENTS.md`](AGENTS.md) — the rules (commits, the clippy gate, licensing, no AI attribution).
2. [`ARCHITECTURE.md`](ARCHITECTURE.md) — daemon + thin clients, crate map, the backend trait seam.
3. [`DECISIONS.md`](DECISIONS.md) — 14 ADRs: *why* each choice, and what to revisit.
4. [`docs/06-build-plan.md`](docs/06-build-plan.md) — milestone status board.

## Where things stand
Everything that can be built/verified on macOS is done and green (CI: clippy + tests + cargo-deny): all five
crates, the engine end-to-end (mock UCS + stub backend), the D-Bus service, the CLI, the GTK4/libadwaita
window + ksni tray, packaging, and the browser-SSO *mechanism* (PKCE + loopback, tested). 30 tests.

**Nothing below has been run against a real Linux system or a real UniFi account yet** — that's this queue.

## First move: bring it up (30 min)
```sh
git clone https://github.com/jakobhviid/tern && cd tern
./packaging/install-local.sh                 # build + install into ~/.local  (needs gtk4/libadwaita — see docs/07 §0)
systemctl --user enable --now tern.service
tern status                                  # → "Not signed in"
tern-gui                                      # window + tray (GNOME: install the AppIndicator extension)
```
This proves the daemon ↔ CLI ↔ GUI ↔ tray plumbing on Linux with no account. Full detail: `docs/07` §1–3.

## Work queue (in order)

### 1. Pin the real SSO params  ⟶ unblocks login
- **Do:** capture the macOS *UniFi Endpoint* app's login with `mitmproxy` (docs/07 §4). Confirm the authorize
  endpoint, token endpoint, `client_id`, scopes, and redirect handling.
- **Then edit:** `crates/tern-core/src/auth.rs` → `AuthConfig::default()` (currently UNCONFIRMED placeholders).
  Also confirm whether it's true OAuth/OIDC or the `/api/sso/v1/...` sequence in `docs/02`; adjust `auth.rs`
  and/or `ucs.rs` accordingly.
- **Verify:** `tern login` opens the browser, completes (passkey OK), and `tern status` shows signed-in.
- **Gotcha:** the loopback redirect must be an allowed `redirect_uri`; if not, fall back to the
  `identity-standard://` scheme (ADR-0009). Passkeys only work via the *system browser* — never an embedded webview.

### 2. Confirm the UCS wire shapes  ⟶ unblocks connect
- **Do:** from the same capture, confirm the request/response JSON of `identity/public_key`,
  `user-token/hosts`, and `POST .../vpn/session`.
- **Then edit:** `crates/tern-core/src/{model.rs,ucs.rs}` — tighten the structs (field names are HIGH
  confidence, exact nesting MEDIUM). The **drive-list endpoint** (`ucs::UcsClient::drives`) is a *guess* —
  find the real one; the engine treats failure as "no drives", so it's low-risk to fix.
- **Verify:** `tern-core` tests still pass; add fixtures from the real payloads.

### 3. Make the VPN actually connect  ⟶ `crates/tern-linux/src/nm.rs`
- **Do:** with a real `vpn/session` config, confirm the wg-quick render imports cleanly:
  `nmcli connection import type wireguard file …` → `up`. Fix field formatting if NM is picky.
- **Verify:** `nmcli connection show --active` lists `tern`; you can reach a LAN host. Confirm the
  **user-owned** connection toggles **without a password** (`connection.permissions user:$USER`).
- ⚠️ **Network safety:** bringing the tunnel up changes routing on the dev box — expected, just be aware.

### 4. Make drives mount  ⟶ `crates/tern-linux/src/gvfs.rs`
- **Do:** confirm `gio mount smb://…` works for your UNAS shares. Wire **file-service credentials from the
  keyring** for authenticated shares (currently non-interactive → fails on auth).
- **Verify:** ticking a drive in the GUI mounts it; it appears in Files; unticking unmounts.

### 5. Real reachability  ⟶ `crates/tern-linux/src/reach.rs`
- Replace the WAN-probe stub with a proper LAN-vs-VPN check that probes the **drive's** SMB host:445.

### 6. Flatpak for Flathub  ⟶ ADR-0014
- **Do:** port the VPN backend from `nmcli` to **D-Bus/NetworkManager** (`nmcli` isn't in the sandbox) as a
  new backend behind `VpnBackend`; select it under Flatpak. Generate `cargo-sources.json`
  (`flatpak-cargo-generator`) for the offline build; then `flatpak-builder packaging/flatpak/phd.hviid.Tern.yaml`.
- **Verify:** the Flatpak connects + mounts inside the sandbox.

### 7. Polish (as time allows)
- **Auto-reconnect** — monitor NM state, re-establish on drop (Mac app's "Auto Reconnect").
- **Multi-site picker** — the GUI/switch currently defaults to the first console (`engine.connect("")`); add a
  site chooser (expose `hosts()` in the snapshot + a dropdown).
- **Release CI + Homebrew tap** — build the `tern` CLI musl bottle + push a generated formula to
  `jakobhviid/homebrew-tap`. **Gated:** owner said don't push to the tap yet.

## Rules that bite (full list in AGENTS.md)
- **Commits:** Conventional Commits, lowercase; **no `Co-Authored-By` / AI attribution**; author as the owner.
- **Gate:** `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` must pass. No fmt gate.
- **Licensing:** enforced by `cargo deny check` — never in-process-link sing-box or libsmbclient; no OpenSSL.
- **User-facing text:** plain language only (docs/05). A test forbids jargon in error titles.
- **Backends:** add a new one by implementing the trait in `tern-core::backend` and selecting it in
  `ternd::build_engine` — no `tern-core` changes.

## Verify your work
```sh
export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:$(brew --prefix)/share/pkgconfig"  # if GTK via brew
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check licenses bans sources
cargo run -p tern-core --example flow        # engine flow demo (no account)
```

## Known unknowns
- Exact SSO protocol + `client_id` (task 1). UCS request/response JSON shapes (task 2). The drive-list
  endpoint path (task 2). Whether UID Enterprise's `vpn/session` differs from consumer Teleport (docs/02).
