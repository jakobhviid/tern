# UX, User-Communication & Error-Handling Guidelines

> This is a **consumer desktop app**, not a network tool. The person using it may not know what WireGuard,
> SMB, D-Bus, or a "console" is — and never needs to. These guidelines are the north-star for every screen,
> message, and failure. When in doubt, optimize for "a non-technical person understands what's happening and
> what to do next," and defer to the **GNOME Human Interface Guidelines** (developer.gnome.org/hig) and
> libadwaita patterns.

## 1. Design principles

1. **Hide the machinery.** The user thinks in three things: **their account**, **access to work/home
   resources**, and **their drives**. WireGuard, SigV4, UCS sessions, ICE, netstack — all invisible.
2. **Plain language, always.** No protocol names, no host IDs, no HTTP codes, no acronyms in anything the user
   reads. "Couldn't reach your network" — not "wg handshake timeout / consoleStandardId unreachable".
3. **Every error ends with a next step.** A message the user can't act on is a bug. Always answer: *what
   happened, in one line* → *what you can do* → *a button that does it* (Retry / Sign in again / Open settings).
4. **State is always visible and honest.** The user should never wonder "is the VPN on?" Show it plainly; never
   show "Connected" when traffic isn't actually flowing.
5. **Quiet by default.** Notify on things the user must know or act on. Never narrate routine success on a loop.
6. **Ask for as little as possible, and explain why** at the moment you ask (keyring, autostart, location-free).
7. **Native and consistent.** Adwaita widgets, system light/dark, keyboard-navigable, screen-reader labels.
   It should feel like it came with GNOME.

## 2. The mental model we present

Collapse the internals into **three user-facing objects**:

| User sees | Internally is | Never surfaced |
|---|---|---|
| **"You're signed in as jah@…"** | SSO session + JWT + device credential enrollment | tokens, MFA transport, IdP URLs |
| **"Access: On / Off"** (a single toggle) | UCS `vpn/session` → WireGuard tunnel via NetworkManager | wg keys, endpoints, allowed-IPs, split-tunnel routes |
| **"Your drives"** (a checklist that mounts) | SMB/GVfs mounts gated on reachability | smb://, cifs, consoleId, mount coordinators |

Advanced details (split-tunnel routes, which server, technical logs) live behind an **"Advanced" / "Details"**
disclosure or a copyable diagnostics view — present for the power user, absent from the default path.

## 3. Information architecture

**Top-bar icon menu** (the fast path — most interactions happen here):
- Header: account identity + overall status line ("Access on · 2 drives mounted").
- **Access** toggle (the VPN, in human words) with a live status line.
- **Drives**: each selected drive with a state dot + click-to-open; "Mount all".
- Quick links: "Open management site", "Settings", "Sign out".

**Main window** (libadwaita `Adw.PreferencesWindow`-style, opened from the menu):
- **Account** page — who you're signed in as, org, session status, "Sign out".
- **Access** page — the toggle, "Connect at startup", "Reconnect automatically", a plain explanation of what
  turning it on does, and an **Advanced** row (split-tunnel/route info, server) collapsed by default.
- **Drives** page — the list of available drives with checkboxes ("mount automatically"), per-drive status,
  and a one-line reachability explainer. This is where the **selective per-drive auto-mount** lives.
- **Settings** page — startup, notifications, keyring, theme follows system.
- **About / Help** — version, diagnostics ("Copy details for support"), links.

## 4. State model — internal → human

Every internal state maps to exactly one **label + icon tint + available action**. Never leak the internal name.

| Internal | Top-bar label | Icon | User action offered |
|---|---|---|---|
| No session | "Not signed in" | outline / grey | **Sign in** |
| SSO in progress | "Signing you in…" | pulsing | (spinner; Cancel) |
| Session valid, access off | "Signed in · Access off" | neutral | **Turn on access** |
| Requesting UCS vpn/session | "Turning on access…" | pulsing | (spinner; Cancel) |
| WireGuard up, verified | "Access on" | accent/green | **Turn off** |
| Tunnel up but no data / DNS fail | "Access on, but not working" | warning/amber | **Reconnect**, **Details** |
| Session expired | "Session expired" | warning | **Sign in again** |
| Console/network unreachable | "Can't reach your network" | warning | **Retry**, **Details** |
| Drive reachable, not mounted | "Ready to mount" | neutral dot | **Mount** |
| Drive mounted | "Mounted" | filled dot | **Open**, **Unmount** |
| Drive selected but unreachable | "Unavailable — turn on Access" | grey dot | **Turn on access** |
| Drive creds rejected | "Sign-in needed for this drive" | warning dot | **Enter credentials** |

**Reachability, in human words** (mirrors the Mac app's "Local Network Connected / Not Mounted", minus the
proprietary relay): "On your network" / "Away — turn on Access to reach your drives" / "Connected via Access".
Do **not** say "VPN-side network" or expose that a drive is only reachable through the tunnel in jargon.

## 5. The top-bar icon

- **Three visual states only:** off/neutral, working (pulsing), on (accent) — plus a small **warning badge**
  for the amber "on but not working" / "can't reach" cases. Don't encode more than a human can read at a glance.
- Tooltip = current status line in words.
- **Reality check (see doc 03):** on Fedora/vanilla GNOME the icon needs the AppIndicator extension. If we
  detect no SNI host, **don't fail silently** — show a first-run card: "To keep the icon in the top bar,
  install the AppIndicator extension" with a one-click link, and keep the app **fully usable from its window**
  regardless (the tray is a convenience, not the only door).

## 6. Notifications policy

Notify **only** when the user needs to know or act:
- ✅ "Access turned off unexpectedly — reconnecting" (then a resolution, not a stream).
- ✅ "Session expired — sign in to stay connected." (actionable)
- ✅ "Couldn't mount *Design* — retry." (actionable, with a Retry action on the notification)
- ✅ First successful setup: one welcome/confirmation.
- 🚫 Every routine connect/disconnect the user initiated (the UI already shows it).
- 🚫 Repeated identical failures — collapse/deduplicate; escalate wording instead of repeating.
- Respect Do-Not-Disturb; use the notification **portal** under Flatpak so actions survive app restarts.

## 7. Error-handling framework

**The pattern for every error the user sees:**
> **One-line plain summary** (what happened) → *optional one line* (what it means / what to do) →
> **primary action button** (the fix) + optional **Details** (copyable technical info for support).

**Rules:**
- **Never** show raw exceptions, HTTP status, D-Bus errors, `wg` output, host IDs, or stack traces in the main
  message. They go **only** inside "Details / Copy for support".
- **Map, don't pass through.** Translate each internal failure to a human cause + recovery (catalog below).
- **Distinguish "your side" vs "their side".** If it's our/network's fault, apologize + auto-retry. If the user
  must act (sign in, enter a code, contact admin), say so clearly.
- **Fail toward the fix.** The button should perform the recovery, not just dismiss.
- **Degrade gracefully.** If drives can't mount, Access can still be on; if the tray can't render, the window
  still works; if the keyring is locked, prompt to unlock rather than dying.

### Error catalog (internal cause → what the user sees)

| Internal cause | User message | Primary action |
|---|---|---|
| SSO login rejected (bad org/domain) | "We couldn't find that organization. Check the address from your invitation." | Re-enter / Open invite |
| MFA required | "Enter the verification code from your authenticator." | Code field |
| MFA push pending | "Approve the sign-in request on your phone." | (waiting; Resend) |
| Session/JWT expired | "Your session expired. Sign in again to stay connected." | **Sign in again** |
| Account locked/disabled/expired (server says) | *Reuse the server's plain reason:* "Your account is locked. Contact your admin." | Contact admin / Copy |
| UCS `vpn/session` fails / no console for user | "Your network isn't available right now. Try again in a moment." | **Retry** + Details |
| WireGuard handshake timeout | "Couldn't connect to your network. It may be offline or unreachable." | **Retry** + Details |
| Tunnel up, DNS/routing broken | "Access is on but not working. Reconnecting may help." | **Reconnect** + Details |
| Gateway only reachable via UI relay (out of scope) | "This network can't be reached from here yet." (honest, not a fake success) | Details / Help |
| NetworkManager absent on host | "This system needs NetworkManager to manage the connection." | Help link (native-package path) |
| Password-prompt/polkit denied | "Permission was needed to change the connection and wasn't granted." | **Try again** |
| Drive host unreachable | "*Design* is unavailable. Turn on Access to reach it." | **Turn on access** |
| SMB credentials rejected | "Sign-in needed for *Design*. Enter your file access credentials." | Credentials sheet |
| Encrypted drive locked | "*Design* is locked. Ask your admin to unlock it." | Contact admin |
| Keyring locked/unavailable | "Unlock your keyring to save your credentials." | **Unlock** |
| Offline (no internet) | "You're offline. We'll reconnect when you're back." | (auto-retry) |
| App update signature fail | "The update couldn't be verified and wasn't installed." | Retry / Help |

> Tone model: the Mac app's own copy is a good target ("Unable to mount X. Please try again later or contact
> your admin.", "Reauthenticate to keep your session active."). We write our own strings in that register —
> calm, plain, actionable — and localize (the Mac app ships ~25 languages; plan for i18n from day one).

## 8. First run / onboarding

- Enter via **invitation link** (`identity-standard://`) or **organization address** — mirror the Mac app's two
  paths, in plain words ("Paste the link from your invitation email, or enter your organization's address").
- **Explain each permission at the moment of asking**, tied to a benefit: keyring ("so you don't re-enter your
  password"), autostart ("so Access is ready when you sign in"), notifications.
- If the top-bar icon can't show (no SNI host), surface the extension nudge here (see §5).
- End with a single clear "You're all set" confirmation and a pointer to the toggle.

## 9. Drives UX (the selective-mount feature)

- Show the user's drives as a **checklist**: tick the ones to mount automatically (persisted per account).
- Each row: name, state dot (§4), and a plain reachability line. Clicking a mounted drive **opens it in Files**.
- Mounting is **reachability-driven**: mount when on the network or when Access is on; unmount cleanly when it
  goes away — never leave a dead mount that hangs Files.
- Be explicit and honest about the boundary: away-from-home + Access off = drives show "Unavailable — turn on
  Access". We do **not** fake the proprietary no-VPN remote path (doc 01/02).
- Never block the UI on a mount; do it in the background with the row spinner, and surface failures per §7.

## 10. Permissions & trust

- **Least privilege, and say so.** The app runs unprivileged (VPN via NetworkManager, drives via GVfs) — good
  for trust; reflect that in an honest "Privacy/Permissions" section (what we store, where; nothing leaves the
  device except talking to your organization's service).
- Secrets live in the **system keyring** — never in plaintext config, never in logs.
- Diagnostics are **opt-in** and **copyable**, and must be scrubbed of tokens before display/export.

## 11. Accessibility & platform conventions

- Full keyboard navigation; every control has an accessible name; respect reduced-motion (no pulsing if the
  user disabled animations).
- Follow system light/dark; sufficient contrast for the state colors (don't rely on color alone — pair the
  amber/green with an icon shape + text, for color-blind users).
- Text scales; layouts use Adwaita adaptive patterns; nothing hard-coded to a pixel width.

## 12. Never show the user (anti-patterns)

- Protocol/impl names: WireGuard, SMB/CIFS, D-Bus, SigV4, ICE/STUN, netstack, "UCS", "sd-wan".
- Internal identifiers: `consoleStandardId`, host UUIDs, session IDs, interface names (`wg0`), IPs/subnets.
- Raw errors: HTTP codes, JSON bodies, `nmcli`/`wg` stderr, polkit action names, stack traces.
- "Success" that isn't (never claim connected/mounted until verified end-to-end).
- Walls of options on the default path — advanced controls stay behind a disclosure.
- A dead-end message with no action.

---

**One-line summary:** the user should be able to run this whole app knowing only *"I'm signed in, Access is on,
my drives are here"* — and whenever something breaks, read one calm sentence and press one button that fixes it.
