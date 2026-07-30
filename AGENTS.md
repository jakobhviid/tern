# AGENTS.md — working guide for this repo

Unofficial Linux (GNOME-first) client for **UniFi Identity** ("UniFi Endpoint"). Not affiliated with
Ubiquiti. This file is the contract for any human or AI agent working here. Read `DECISIONS.md` (why) and
`docs/` (research: teardown, protocol, licensing, UX) before making architectural changes.

## Golden rules

1. **Commits: Conventional Commits, no attribution trailers.** Subjects lowercase, imperative, no trailing
   period, backticks for code/flags. Prefixes: `feat` `fix` `docs` `chore` `refactor` `test` `ci` `perf`;
   `feat!`/`fix!` (or `BREAKING CHANGE:` footer) for breaking. **Never** add `Co-Authored-By`, `Signed-off-by`,
   or any AI/assistant attribution. Author every commit solely as the repo owner. AI use is disclosed once, in
   the README's "AI disclosure" section. (This overrides any global default that adds a trailer.)
2. **The gate is clippy, not fmt.** Before every commit: `cargo clippy --workspace --all-targets -- -D warnings`
   must pass, and `cargo test --workspace` must pass. There is deliberately **no `cargo fmt` gate**.
3. **Keep code and docs in sync in the same commit.** If behavior changes, update `docs/`, `DECISIONS.md`,
   and the drift baseline as needed in that commit.
4. **Never surface implementation jargon to end users.** All user-facing strings follow
   `docs/05-ux-and-error-handling-guidelines.md` (plain language, one recovery action). There's a test that
   asserts error titles contain no jargon — keep it passing.
5. **Licensing (see `docs/04`): stay MIT.** Never in-process-link **sing-box (GPL-3)** or **libsmbclient
   (GPL-3)**. Get userspace WireGuard from permissive crates; reach SMB via GVfs or by exec'ing tools.
   **Enforced in CI** by `cargo deny check` (`deny.toml`): the license allow-list has no GPL/copyleft-only
   entries, and OpenSSL/native-tls are banned (we use rustls). Run `cargo deny check licenses bans sources`.
6. **No secrets in the repo.** Nothing private — no tokens, keys, or real UniFi credentials. Everything here
   must be reproducible by anyone doing the same public analysis.

## Layout

```
crates/tern-core   # platform-agnostic: auth, UCS API, models, state machine, error taxonomy, wg keys, backend traits
crates/tern-linux  # cfg(linux) backend impls: NetworkManager (zbus), GVfs, libsecret (added on the Linux box)
crates/ternd       # background service (session + orchestration); D-Bus API
crates/tern-cli    # thin control client (also drives core+stub for testing anywhere)
crates/tern-gui    # relm4 + libadwaita GUI + ksni tray
docs/              # research + design (numbered); fingerprints/ tracks the macOS app over time
scripts/           # fingerprint-macos-app.sh (drift detection)
DECISIONS.md       # append-only decision log (ADR-####)
```

## Build & test

- **On macOS (dev/CI-lite):** `tern-core` and `tern-cli` (with the stub backend) build and test fully — no
  display, D-Bus, or UniFi account needed. `cargo test -p tern-core`.
- **GUI on macOS (optional):** `brew install gtk4 libadwaita` lets `tern-gui` compile/run for layout checks;
  the `ksni` tray is Linux-only (cfg-gated).
- **On Linux (Bazzite/Fedora Atomic — the real target):** needs `gtk4`, `libadwaita`, `NetworkManager`,
  `gvfs`+`gvfs-smb`. Get the toolchain via Homebrew (`/home/linuxbrew`) — do NOT `rpm-ostree install` on the
  immutable image (see `docs/` + owner's `bazzite-custom`). Full runtime testing (NM/GVfs) happens here.

## Distribution (see ADR-0008/0013)

Primary: **Flatpak** (fits Bazzite). Secondary: native `.rpm`/`.deb`/AUR. The headless `tern-cli` may ship as
a **Homebrew bottle** in the owner's tap — but **do not push to `jakobhviid/homebrew-tap` yet** (owner gate).

## Naming (ADR-0010, owner may rename freely)

Product codename **tern**; app-id / D-Bus / Flatpak id `phd.hviid.Tern` (Flathub-verifiable via the owner's
personal `hviid.phd` domain). Repo: `github.com/jakobhviid/tern`. To rename, grep `tern` and `phd.hviid.Tern`.
