# GUI Design — the `tern-gui` window & tray

> This is the concrete, buildable design for **tern-gui** (GTK4 + libadwaita + ksni tray). It is a *thin client*
> of `ternd`: it renders the [`Snapshot`](../crates/tern-core/src/state.rs) the daemon emits and calls the
> daemon's D-Bus methods — it holds **no** session/VPN logic of its own. Read alongside
> [`docs/05`](05-ux-and-error-handling-guidelines.md) (the mandatory UX rules), `ARCHITECTURE.md` (the
> daemon+thin-client shape), and ADR-0006 / ADR-0009 / ADR-0016 in `DECISIONS.md`.
>
> **Scope of change vs. today:** the current window still drives the dead browser-OAuth "Sign in" button
> (ADR-0009 revised — that path does not exist for a consumer account). This design **replaces onboarding with a
> Teleport-invite paste flow** (ADR-0016) and fills in every state the `Snapshot` can express. It also lists the
> **new D-Bus methods** the daemon must add, because the current interface predates Teleport.

## 1. Principles (inherited, not restated)

Everything in `docs/05` holds verbatim: hide the machinery; plain language only; every error ends in exactly one
recovery action; state is honest (never fake "connected"); quiet by default; Adwaita-native, keyboard- and
screen-reader-friendly, follows system light/dark. This document only says **how the widgets realize those
rules** for each state. The three user-facing objects stay: **your account/console**, **Access (on/off)**,
**your drives**.

## 2. Information architecture

Two surfaces, one source of truth (the `Snapshot`):

- **Tray icon (ksni / StatusNotifierItem)** — the fast path. Icon tint + tooltip + a short menu. Already present
  and largely correct; §7 lists the edits.
- **Main window (`AdwApplicationWindow`)** — the full surface. One window whose *body swaps by state*, plus a
  secondary **Preferences window** for the rarely-touched settings.

The tray is a **convenience, not the only door** (docs/05 §5): on vanilla GNOME with no SNI host the window must
be fully usable on its own, and we surface the AppIndicator nudge (§8).

### 2.1 Window skeleton

```
AdwApplicationWindow  (application_id = phd.hviid.Tern, ~440×560, resizable)
└─ AdwToastOverlay                         ← all transient errors / confirmations land here
   └─ AdwToolbarView
      ├─ [top]  AdwHeaderBar
      │          title: "Tern"
      │          end:   ⋮  primary menu (GMenu → MenuButton)
      │                    • Preferences        (opens the AdwPreferencesWindow)
      │                    • Reconnect           (visible only when Access is On/Degraded)
      │                    • Sign out / Forget this console
      │                    • About Tern
      └─ [content]  a single child that is REPLACED per state (see §4):
                     one of  { InviteView, PairingView, MainView, ServiceDownView }
```

Why one swappable child instead of an `AdwViewStack` of tabs: the states are *mutually exclusive phases*
(not-onboarded → pairing → onboarded), so a stack of user-switchable pages would be wrong. Set the child from the
snapshot handler (raw gtk4-rs: `toolbar.set_content(Some(&view))`; relm4: a `#[transition]` on a view-model enum).

### 2.2 Preferences window

`AdwPreferencesWindow` opened from the primary menu — this is where docs/05 §3's "pages" live, kept out of the
default path:

- **Account/Console page** — which console you're paired with, session status, **Sign out / Forget this console**,
  and (later) a **Pair another console** button.
- **Access page** — `AdwSwitchRow` "Connect at startup", `AdwSwitchRow` "Reconnect automatically", a one-line
  plain explanation of what Access does, and an **Advanced** `AdwExpanderRow` (server/route summary, copyable),
  collapsed by default.
- **Drives page** — the full drive checklist (the same rows as the main view, authoritative here) with the
  reachability explainer line.
- **General page** — start at login, notifications, "follow system theme" (default on), keyring info.
- **About** — `AdwAboutWindow`: version, "Not affiliated with Ubiquiti", **Copy details for support** (scrubbed
  diagnostics), links.

The main window carries the *live/at-a-glance* controls (Access toggle + drive list); Preferences carries
*configuration*. The drive list appears in both — the main view for speed, Preferences as the settings home.

## 3. The mapping the whole GUI is built on

`Snapshot { auth, access, drives }` fully determines what renders. Auth selects the **view**; within
`SignedIn`, Access + drives fill the **MainView**.

| `Snapshot` value | View / widget that renders it |
|---|---|
| `auth = SignedOut` | **InviteView** — `AdwStatusPage` + invite entry (§4.1) |
| `auth = SigningIn` | **PairingView** — spinner "Pairing with your console…" (§4.2) |
| `auth = SignedIn(id)` | **MainView** — summary + Access group + Drives group (§4.3) |
| `auth = SessionExpired` | **InviteView**, banner variant "Your access expired — paste a new invite" (§4.6) |
| actor `Disconnected` | **ServiceDownView** — `AdwStatusPage` "Background service not running" (§4.7) |
| `access = Off` | Access `AdwSwitchRow` off, subtitle "Off" |
| `access = TurningOn` | Switch on + insensitive; subtitle "Turning on…"; header spinner |
| `access = On` | Switch on; subtitle "On"; drives become mountable |
| `access = Degraded` | Switch on; **amber warning row** "On, but not working" + **Reconnect** (§4.5) |
| `access = Unreachable` | Switch reflects intent; **amber warning row** "Can't reach your network" + **Retry** |
| `DriveMount::*` (per row) | `DriveMount::label()` verbatim as the row subtitle; suffix control per §6 |
| `DriveStatus.selected` | the row's auto-mount `AdwSwitchRow` active state |
| `Snapshot::summary_line()` | the big status label at the top of MainView **and** the tray tooltip |
| `Snapshot::tray_visual()` | the tray icon name (§7) |

**One rule to prevent divergence:** the snapshot handler is a single pure `render(&Snapshot)` that rebuilds/updates
these widgets. Never mutate widgets from a click handler except to send a `Cmd`; the resulting `Changed` signal
re-renders. (This is why the current code needs the `updating: Cell<bool>` guard — see §9.)

## 4. Screens & states

### 4.1 Not onboarded — InviteView (`auth = SignedOut`)

Replaces the dead "Sign in" button. An `AdwStatusPage` centered in the window.

```
┌────────────────────────────────────────────┐
│  Tern                                    ⋮  │
├────────────────────────────────────────────┤
│                                            │
│              (network icon)                │
│           Connect to your network          │
│   Paste the Teleport invite link from your │
│   console's VPN settings to get started.   │
│                                            │
│   ┌──────────────────────────────────────┐ │
│   │ https://teleport.ui.link/…           │ │  ← AdwEntryRow "Invite link"
│   └──────────────────────────────────────┘ │
│      ⓘ Find it in your console:            │  ← dim helper label
│         Settings → VPN → Teleport          │
│                                            │
│              [   Pair   ]                  │  ← suggested-action; insensitive until valid
│                                            │
└────────────────────────────────────────────┘
```

- **Widget:** `AdwStatusPage` (icon `network-vpn-symbolic`, title, description) whose child is an
  `AdwPreferencesGroup` holding one `AdwEntryRow` ("Invite link") + a **Pair** button (`suggested-action`).
- **Live validation (local, zero round-trip):** `tern-gui` already links `tern-core`, so validate on every
  keystroke with [`teleport::Invite::parse`](../crates/tern-core/src/teleport.rs). Valid → enable **Pair** and
  show a subtle success (checkmark in the row's suffix). Invalid-but-nonempty → keep **Pair** disabled and, only
  after the field loses focus or on a failed submit, show the row `.error` style + the plain line
  *"That link doesn't look right. Copy it again from your console."* (mirrors `Error::InvalidInvite`'s
  `user_facing`). Empty → neutral, no error. Accept a full `https://teleport.ui.link/<uuid>` URL **or** a bare
  UUID (parse already tolerates both, plus stray query/whitespace).
- **Paste affordance:** the entry auto-selects on focus; support Ctrl-V normally. (Optional: a small
  "Paste" suffix button that reads the clipboard — nice-to-have.)
- **Action:** **Pair** → `Cmd::RedeemInvite(text)` → daemon `RedeemInvite(url)` (NEW method, §5). Do **not**
  pre-strip/normalize in the GUI beyond trimming; the daemon re-parses authoritatively.

### 4.2 Pairing — PairingView (`auth = SigningIn`)

```
│              (spinner)                     │
│          Pairing with your console…        │
│        This only takes a moment.           │
│                                            │
│              [  Cancel  ]                  │
```

- `AdwStatusPage` with a `gtk::Spinner` (or `AdwSpinner`) as the icon slot; respect reduced-motion (docs/05 §11 —
  if animations are disabled, show a static "in progress" state, no pulsing).
- **Cancel** → `Cmd::CancelSignIn` → daemon `CancelPairing()` (NEW, §5) which calls the engine's existing
  `cancel_sign_in()` and returns to `SignedOut`.
- Failure (bad/expired/single-use invite already redeemed, or broker unreachable) comes back as the
  `RedeemInvite` **ActionResult.error** → render as a toast/inline error on the InviteView we fall back to
  (see §4.6). Single-use is the common one: *"That invite has already been used. Generate a new one in your
  console."* — this is a **NEW `UserFacing`** the daemon should return (see §5, error additions).

### 4.3 Onboarded — MainView (`auth = SignedIn`)

The everyday screen. Uses `AdwPreferencesGroup`s in a vertical box (or `AdwPreferencesPage` for scroll + clamp).

```
┌────────────────────────────────────────────┐
│  Tern                                    ⋮  │
├────────────────────────────────────────────┤
│  Access on · 2 drives mounted              │  ← title-2 label = Snapshot::summary_line()
│  Home console                              │  ← dim caption = console name (from Hosts)
│                                            │
│  Access                                    │  ← AdwPreferencesGroup title
│  ┌──────────────────────────────────────┐  │
│  │ Access                    [ ●───]     │  │  ← AdwSwitchRow, subtitle = access_subtitle()
│  │ On                                    │  │
│  └──────────────────────────────────────┘  │
│                                            │
│  Your drives                               │  ← AdwPreferencesGroup title
│  ┌──────────────────────────────────────┐  │
│  │ Design            Mounted     [●──] ⌞⌝│  │  ← AdwSwitchRow(auto-mount)+Open suffix
│  │ Shared            Mounted     [●──] ⌞⌝│  │
│  │ Archive   Unavailable — turn on Access│  │  ← subtitle from DriveMount::label()
│  │                                [──○]  │  │
│  └──────────────────────────────────────┘  │
└────────────────────────────────────────────┘
```

- **Summary label** — `title-2`, from `summary_line()`. **Console caption** below it — dim label with the paired
  console's `name` (fetch via the existing `Hosts` method; if unknown, omit the line rather than show an id).
- **Access group** — a single **`AdwSwitchRow`** titled "Access" (replace the current `AdwActionRow` +
  standalone `Switch`; `AdwSwitchRow` is the HIG-correct widget and gives a built-in accessible label). Subtitle
  = `access_subtitle(access)`. While `TurningOn`, set the row insensitive and show a header spinner; never let the
  toggle flip back mid-transition.
- **Drives group** — one row per `DriveStatus` (§6). Empty list → `AdwActionRow` placeholder "No drives on this
  console." (see §4.8).

### 4.4 Connecting / everyday connect-disconnect

- Flipping Access **on** → `Cmd::Connect("")` (empty console id = the paired/only console; the engine already
  accepts this and the tray already sends it). Row → insensitive, subtitle "Turning on…", tray → Working.
- On success the `Changed` snapshot arrives with `access = On`; drives that are `Reachable`/selected transition to
  `Mounting` → `Mounted` on their own. No notification (user-initiated success is silent, docs/05 §6).
- Flipping **off** → `Cmd::Disconnect`. Mounted drives unmount cleanly and drop to `Unavailable`/`Idle`.

### 4.5 Degraded / Unreachable (`access = Degraded | Unreachable`)

Do not show a bare toggle that lies. Insert an **inline warning banner** above the Access group and keep the
switch honest:

```
│  ⚠ Access is on but not working.           │  ← AdwBanner (revealed), .warning
│    Reconnecting may help.      [Reconnect]  │
```

- `AdwBanner` (title + one button). `Degraded` → button **Reconnect** (`Cmd::Reconnect`, see §5); `Unreachable`
  → button **Retry** (re-issues `Connect`). Text comes from the catalog (`VpnDegraded` / `VpnUnreachable`
  `user_facing().title`).
- Colour is paired with an **icon + text**, never colour alone (docs/05 §11, colour-blind safety).
- Tray tint → Warning; a **single** notification fires on the *transition into* the bad state (the current code
  already does this and de-dupes via `prev_access`) — keep that, don't narrate on a loop.

### 4.6 Session expired (`auth = SessionExpired`)

The saved reusable session is no longer valid → we need a fresh invite (Teleport invites are single-use, so we
can't silently re-pair). Render the **InviteView** with a persistent `AdwBanner` at the top:

```
│  ⚠ Your access expired. Paste a new invite  │
│    from your console to reconnect.          │
```

- Matches `Error::SessionExpired`'s intent but in Teleport terms (a new invite, not a password re-login).
- A **notification** fires once on entering `SessionExpired` (docs/05 §6 — actionable). The current code already
  notifies here; keep it, reword to "Paste a new invite to reconnect."

### 4.7 Service down (actor `Disconnected`)

`ternd` not running / no session bus. Not a `Snapshot` state — it's the actor telling the GUI the pipe is dead.

```
│           (dialog-error icon)              │
│      Background service isn't running       │
│   Tern's helper needs to be running to      │
│   manage your connection.                   │
│              [ Try again ]                  │
```

- `AdwStatusPage`. **Try again** re-attempts the D-Bus connection (`Cmd::Refresh` re-drives the actor's connect).
  Under a systemd `--user` unit this is usually transient; if we can, offer a secondary "how to start it" help
  link rather than a raw `systemctl` string.
- Tray, if it was up, drops to Neutral with tooltip "Background service not running".

### 4.8 Empty & loading states

- **Loading (first paint, before the first snapshot):** show a neutral MainView shell with the summary label
  reading "Checking…" and controls insensitive — never a blank window. Replaced the instant the first snapshot
  lands (the actor pushes it immediately on connect).
- **No drives on this console:** Drives group shows one dim `AdwActionRow` "No drives on this console." — not an
  error, no action.
- **Drives present but Access off:** rows render with subtitle "Unavailable — turn on Access" and their suffix is a
  small **Turn on Access** affordance (or rely on the top toggle) — the row is honest, not hidden.

## 5. D-Bus interface: what the GUI calls, and what's MISSING

Current interface (`phd.hviid.Tern.Daemon`, JSON-string methods + `Changed` signal): `Snapshot`, `Hosts`,
`StartSignIn`, `CompleteSignIn(token)`, `SignOut`, `Connect(console_id)`, `Disconnect`,
`SetAutoMount(drive_id, on)`. That set predates Teleport. This design needs the following changes.

### 5.1 New methods the daemon must add

| New method | Signature (JSON string in/out) | Why the GUI needs it | Maps to engine |
|---|---|---|---|
| **`RedeemInvite`** | `RedeemInvite(url: String) -> ActionResult` | The onboarding action. Parse + pair via the Teleport broker, save the reusable session, emit `Changed`. Replaces `StartSignIn`. | `Invite::parse` → broker pair → persist session (keyring) → `Auth::SignedIn` |
| **`CancelPairing`** | `CancelPairing() -> ActionResult` | Back out of PairingView. | existing `engine.cancel_sign_in()` |
| **`Reconnect`** | `Reconnect() -> ActionResult` | The `Degraded` recovery action (distinct from off→on). | disconnect+connect the current session |
| **`AuthorizeConnection`** | `AuthorizeConnection() -> ActionResult` | The **one-time privilege grant** (§10). Triggers the polkit prompt so the daemon can create the system-wide TUN. | grant `cap_net_admin` / install polkit rule |
| **`SetDriveCredentials`** | `SetDriveCredentials(drive_id, username, secret) -> ActionResult` | The `CredentialsNeeded` recovery (the credentials sheet, §6). | store in keyring, re-mount |
| **`MountDrive`** / **`UnmountDrive`** | `MountDrive(drive_id) -> ActionResult` / `UnmountDrive(drive_id) -> ActionResult` | Manual "Mount now" / "Unmount" and clean row actions beyond the auto-mount toggle. | `engine.mount_one` / unmount |

`RedeemInvite` and `Reconnect` are the two **must-haves** for this design to function; the rest complete the
drives/privilege UX and can land as those features do. All keep the existing contract: return the `{ok, error}`
`ActionResult` JSON and emit `Changed`.

### 5.2 Methods to remove/retire from the GUI's proxy

- Drop the GUI's `StartSignIn` call and the **"Sign in" button + `Cmd::StartSignIn`** entirely (dead OAuth path,
  ADR-0009 revised). `CompleteSignIn(token)` is likewise not used by the consumer path — leave it on the daemon if
  useful for tests, but the GUI must not reference it.
- `Connect(console_id)` **stays** (empty string = paired/only console). When multi-console lands, a picker fills
  the id; until then the empty-string convention the tray already uses is correct.

### 5.3 New taxonomy the daemon should return (so the GUI stays dumb)

The GUI must not invent copy. Add these to `error.rs` so `RedeemInvite`/`AuthorizeConnection` return them and the
GUI renders `UserFacing.title` + the `UserAction` button verbatim:

- **`InviteAlreadyUsed`** → *"That invite has already been used. Generate a new one in your console."* → a new
  `UserAction` (e.g. `NewInvite`, button "Get a new invite" → returns to InviteView). Teleport invites are
  single-use (see `teleport.rs`), so this is a first-class case, not a generic retry.
- **`PrivilegeRequired`** → *"Tern needs your permission to set up the connection."* → new `UserAction`
  `GrantPermission` (button "Continue" → calls `AuthorizeConnection`). Distinct from the existing `PolkitDenied`
  ("…wasn't granted" → Retry), which is the *user-declined* case.

Everything else the GUI shows already exists in `error.rs` / `state.rs` (the drive states, `VpnDegraded`,
`VpnUnreachable`, `SessionExpired`, `KeyringLocked`, etc.). No per-drive `UserFacing` is needed in the snapshot:
each `DriveMount` variant maps deterministically to one catalog row + one action (§6), so the GUI derives the
message from the state. (If we later want the *exact* server reason surfaced per drive, add an optional
`last_error: Option<UserFacing>` to `Snapshot` — noted as a possible enhancement, not required now.)

### 5.4 How errors reach the UI

Two channels, both already supported by the wire contract:

1. **User-initiated action** → the method's returned `ActionResult`. If `!ok`, render `error` (a `UserFacing`)
   as an **`AdwToast`** (transient) or, for a blocking failure like pairing, inline on the relevant view, with
   its single `UserAction` button wired to the matching `Cmd`. The `detail` field (technical text) goes **only**
   behind "Copy details for support" in the About page — never in the toast.
2. **Background/async state change** → the `Changed` snapshot. `access`/`auth`/per-drive states drive the banners,
   row subtitles, and the (deduplicated) notifications per §4.5/§4.6.

## 6. Drives (the selective-mount feature)

Each `DriveStatus` → one row in the Drives group. The row is an **`AdwSwitchRow`** whose switch is the *auto-mount
selection* (`selected`), title = `drive.name`, subtitle = `DriveMount::label()` (verbatim — already plain
language), plus a **state-dependent suffix**:

| `DriveMount` | Subtitle (from `label()`) | Suffix control | Action → |
|---|---|---|---|
| `Idle` | "Not mounted" | — | (toggle auto-mount) |
| `Reachable` | "Ready to mount" | button **Mount** | `MountDrive(id)` |
| `Mounting` | "Mounting…" | small spinner | — |
| `Mounted` | "Mounted" | **Open** (folder icon) button; row click opens too | `gio`/portal open `smb://…` in Files; **Unmount** in a menu |
| `Unavailable` | "Unavailable — turn on Access" | button **Turn on Access** | `Connect("")` |
| `CredentialsNeeded` | "Sign-in needed" | button **Enter credentials** | opens credentials sheet → `SetDriveCredentials` |
| `Locked` | "Locked" | button **Contact admin** (or info) | help/detail; can't self-resolve (`DriveLocked`) |
| `Failed` | "Couldn't mount" | button **Retry** | `MountDrive(id)` |

- **Reachability-gated:** the auto-mount switch stays interactive regardless (it's a *preference*), but mounting
  only happens when reachable/Access-on — exactly the daemon's job; the row honestly shows "Unavailable — turn on
  Access" until then. Never fake a mount (docs/05 §9).
- **Credentials sheet:** an `AdwPreferencesDialog`/`AdwDialog` (modal to the window) with two `AdwEntryRow`s
  (username + password, the latter `visibility=false`), a "Save to keyring" note explaining *why we ask now*
  (docs/05 §6/§8), and a Save button → `SetDriveCredentials(id, user, secret)`. Secrets go to the keyring via the
  daemon; the GUI never persists them.
- **Open in Files:** clicking a `Mounted` row (or its Open suffix) launches the file manager on the mount via the
  portal (`OpenURI`) or `gio open` — no custom file UI.
- **Failures** surface per §5.4 (a toast with **Retry**), never a modal that blocks the whole window (docs/05 §9).

## 7. Tray (ksni / StatusNotifierItem)

Keep the current `tray.rs` structure (it already reflects `Snapshot` live and routes actions through the same
channels). Edits to match this design:

- **Icon** stays `Snapshot::tray_visual()` → the four themed symbolic names already used
  (`network-vpn-symbolic` / `-acquiring-` / `-no-route-` / `-disconnected-`). Honest three-states-plus-warning per
  docs/05 §5. Respect reduced-motion (no synthetic pulsing).
- **Tooltip** = `summary_line()` (unchanged).
- **Menu**, state-aware:
  - `SignedOut` / `SessionExpired`: **Open Tern** (to pair) · separator · **Quit**. The Access toggle is **absent**
    (nothing to toggle) rather than shown-disabled.
  - `SignedIn`: **Open Tern** · separator · **Turn Access on/off** (label from state; hidden while `TurningOn`,
    or shown as a disabled "Turning on…") · **Reconnect** (only when `Degraded`) · **Sign out / Forget this
    console** · separator · **Quit**.
- Replace the always-present **"Sign out"** with a labelled **"Forget this console"** when signed in; drop it when
  signed out.
- **No SNI host:** the tray fails to spawn today and the window still works (good). Add the **first-run nudge**
  (§8) so the user knows the icon needs the AppIndicator extension — don't fail silently (docs/05 §5).

## 8. Onboarding flow (first run) & the AppIndicator nudge

1. Launch → no saved session → **InviteView** (§4.1).
2. If **no SNI host** was detected at startup, show a dismissible `AdwBanner` at the top of InviteView:
   *"To keep Tern's icon in the top bar, add the AppIndicator extension."* with a **learn more** link. The app is
   fully usable without it.
3. User pastes the invite → **Pair** → PairingView → on success, **MainView**.
4. First successful pairing → **one** welcome toast/notification: *"You're all set — turn on Access when you need
   your network."* (docs/05 §6/§8: a single confirmation, then quiet).
5. First time Access is turned on and the daemon lacks the TUN privilege → the **one-time grant** flow (§10),
   explained at the moment of asking.

Permissions are explained *when asked*, tied to a benefit (docs/05 §8): keyring ("so you don't paste the invite
again"), autostart ("so Access is ready when you sign in"), the connection privilege (§10).

## 9. Everyday flows (summary)

- **Connect / disconnect:** top Access `AdwSwitchRow` (or tray "Turn Access on/off") → `Connect("")` /
  `Disconnect`. Honest transitional state; silent on user-initiated success.
- **Add/mount a drive:** tick its auto-mount switch (`SetAutoMount`) — it mounts when reachable; or press **Mount**
  on a `Reachable` row (`MountDrive`). Open a `Mounted` drive by clicking the row.
- **Handle an error:** read the one-line toast/banner, press its single button (the mapped `UserAction`). Details
  live only behind "Copy details for support" in About.
- **Reconnect a degraded tunnel:** the `Degraded` banner's **Reconnect** button → `Reconnect`.
- **Re-onboard after expiry:** paste a fresh invite on the expired-banner InviteView (§4.6).

## 10. The one-time privilege grant (system-wide TUN)

ADR-0016 (decision b) makes Access a **system-wide TUN**, which needs `cap_net_admin` on the daemon (via
`setcap` at install, or a polkit-mediated grant). GUI responsibilities:

- **Preferred:** the privilege is set at install time (packaging: `setcap` on `ternd`, or a polkit action for the
  systemd `--user` service). Then the GUI never has to ask — the first Connect just works.
- **Fallback / Flatpak / source installs where it isn't pre-granted:** the first `Connect` returns
  `PrivilegeRequired` (§5.3). The GUI shows a calm inline card *before* prompting the OS:
  *"Tern needs your permission to set up the connection. This is asked once."* with a **Continue** button →
  `AuthorizeConnection()`, which triggers the **standard system polkit dialog** (not a Tern-drawn password box).
  On success, Access proceeds; on decline → `PolkitDenied` ("…wasn't granted", **Try again**).
- Never show `cap_net_admin`, `pkexec`, or a polkit action id (docs/05 §12). The honest "runs unprivileged except
  this one grant" story belongs in the About/Permissions section (docs/05 §10).

## 11. GNOME HIG / libadwaita conformance checklist

- Use **Adwaita widgets** as-is, no custom CSS beyond stock style classes (`suggested-action`, `.warning`,
  `title-2`, `boxed-list`): `AdwApplicationWindow`, `AdwToolbarView`, `AdwHeaderBar`, `AdwStatusPage`,
  `AdwPreferencesGroup`, `AdwSwitchRow`, `AdwActionRow`, `AdwEntryRow`, `AdwBanner`, `AdwToast`/`AdwToastOverlay`,
  `AdwPreferencesWindow`, `AdwAboutWindow`, `AdwDialog`.
- **Switch → `AdwSwitchRow`**, not a bare `gtk::Switch` in an `AdwActionRow` (current code): correct HIG widget,
  free accessible label, consistent activation.
- **System light/dark** followed automatically (default libadwaita); "follow system theme" toggle in Preferences.
- **Keyboard + a11y:** every control reachable by Tab; accessible names on icons-only buttons (Open, Paste);
  Escape closes dialogs; the invite entry submits on Enter.
- **Reduced motion:** no spinners/pulsing when the user disabled animations — swap for static progress text.
- **Adaptive:** relative sizing, `AdwClamp`/`AdwPreferencesPage` for width; nothing pinned to a pixel width;
  text scales.
- **No colour-only signalling:** amber/green always paired with an icon shape + words.

## 12. Implementation notes — raw gtk4-rs vs relm4

The current `tern-gui` is **raw gtk4-rs** (ADR-0006 picked relm4 but flagged "drop to raw if friction wins"; the
built code went raw). This design is deliberately **toolkit-neutral at the widget level** — the same Adwaita
widgets apply either way. Guidance:

- **Stay raw gtk4-rs for now.** The surface is small and the three-thread bridge (GTK / tokio actor / ksni) is
  already working. Migrating mid-build buys little.
- Structure the render path as **one `render(&Snapshot)`** that (a) picks the top-level view
  (`SignedOut`→Invite, `SigningIn`→Pairing, `SignedIn`→Main, `SessionExpired`→Invite+banner) and (b) updates the
  MainView's Access row + rebuilds the Drives group. Keep click handlers thin: they only `try_send(Cmd)`.
- **The one wart to preserve carefully:** programmatic switch updates must not echo back as user commands. The
  code uses an `updating: Cell<bool>` guard around `set_active`/`set_state` — keep that discipline for the Access
  and per-drive switches. **This is exactly the boilerplate relm4's MVU removes** (view is a pure function of the
  model, so no echo guard). If the drives/settings surface grows enough that the manual diffing hurts, that's the
  trigger to revisit ADR-0006 and adopt relm4 — model = the `Snapshot` (+ a small view enum), `update` = apply a
  `Changed`, `view` = §3's mapping. Until then, raw gtk4-rs with a single `render` is the pragmatic call.
- **New proxy methods:** extend the `#[zbus::proxy]` trait in `main.rs` with `redeem_invite`, `cancel_pairing`,
  `reconnect`, `authorize_connection`, `set_drive_credentials`, `mount_drive`, `unmount_drive`; add matching `Cmd`
  variants and route them in `actor()`. Remove `start_sign_in` and the "Sign in" button.

## 13. What changes from today's `tern-gui` (concrete diff)

**Remove:**
- The **"Sign in" button**, `signin.connect_clicked`, `Cmd::StartSignIn`, and the `start_sign_in` proxy call
  (dead OAuth path).
- The single-screen assumption (summary + Access + drives always visible even when signed out): the body now
  swaps by `auth`.
- The bare `gtk::Switch`-in-`AdwActionRow` for Access → `AdwSwitchRow`.
- The always-on **"Sign out"** button in the action row (moves to the primary menu + becomes "Forget this
  console"; tray label likewise).

**Add:**
- **InviteView** (`AdwStatusPage` + `AdwEntryRow` + Pair) with **local** `Invite::parse` validation.
- **PairingView** (spinner + Cancel), **ServiceDownView** (already partially present as a label — promote to an
  `AdwStatusPage` with **Try again**).
- **`AdwToastOverlay`** wrapping the content, and **`AdwBanner`s** for degraded/unreachable/expired.
- **Primary menu** (Preferences, Reconnect, Sign out/Forget, About) and an **`AdwPreferencesWindow`** (Account,
  Access, Drives, General, About).
- Per-drive **suffix controls** and the **credentials sheet** (§6).
- The **privilege-grant** card + `AuthorizeConnection` call (§10).
- The **AppIndicator first-run nudge** when no SNI host (§8).
- New `Cmd`s + proxy methods per §5/§12.

---

**One-line summary:** the window becomes a state machine over `Snapshot` — paste-an-invite when signed out, a
calm Access toggle + honest drive checklist when paired, one banner/toast + one button on every failure — and the
daemon gains a handful of Teleport-era methods (chiefly `RedeemInvite` and `Reconnect`) that the current
OAuth-era interface is missing.
