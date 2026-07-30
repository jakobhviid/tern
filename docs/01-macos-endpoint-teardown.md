# UniFi Endpoint (macOS) — Teardown & Findings

> First-hand reverse-engineering of the installed macOS client, to inform a Linux (GNOME-first) port.
> Source: `/Applications/UniFi Endpoint.app`, **v4.1.1 (build 177)**. Evidence = Info.plist, entitlements,
> `otool -L`, bundled system-extension Info.plist, embedded Go symbol strings, and `en.lproj` UI strings.

## 1. What it is

The app that Ubiquiti ships as "UniFi Endpoint" is the **UniFi Identity (UID) desktop client**.

- Bundle id: `com.ui.uid.standard-desktop`
- Type: **menu-bar–only agent** — `LSUIElement = true` (no Dock icon, lives in the macOS menu bar / "top").
- Built as a native **Swift / AppKit** app (some SwiftUI), SDK `macosx26.2`, min macOS 13.0.
- URL scheme: `identity-standard://` (SSO deep-link callback).
- Bonjour: advertises/browses `_lnp._tcp` + `_bonjour._tcp`; `NSLocalNetworkUsageDescription` =
  *"Local network access is needed to expedite the credential obtaining process."*
- App group: `4P645293E8.group.com.ui.uid.desktop` (shared between the app and its helpers).

### Notable bundled frameworks / libs
RxSwift/RxCocoa, SnapKit (layout), **Realm/RealmSwift** (local DB + MongoDB Atlas/Realm sync),
Kingfisher (images), AFNetworking, CocoaLumberjack, **WebRTC.framework**, AWS IoT + AWSCore (device
messaging), Firebase (Core/Crashlytics/RemoteConfig/Sessions). Localized into ~25 languages.

## 2. The four capability clusters (what the user asked to replicate)

### A. Login / SSO
- "Sign in with your web browser" → browser-based OAuth against **`sso.ui.com`** / **`account.ui.com`**.
- Org enrollment: "Sign In to Your Organization" via **Organization domain** or an **invitation email/link**.
- Callback returns to the app via the `identity-standard://` URL scheme.
- Local Bonjour (`_lnp._tcp`) used to "expedite the credential obtaining process" (grab creds from a
  nearby UniFi device / console on the LAN).
- Session reauth prompts: *"Reauthenticate to keep your VPN session active."*
- Backend: UID API gateway **`api-gw.uid.df.ui.com`**, enterprise svc **`enterprise.svc.ui.com`**,
  MongoDB Realm/Atlas (`realm::app` config seen in strings), Firebase + AWS IoT for push/remote config.

### B. One-Click VPN (the WireGuard / "Teleport-style" piece)
- Implemented as an **`NEPacketTunnelProvider` system extension**:
  `Contents/Library/SystemExtensions/com.ui.uid.standard-desktop.network-extension.systemextension`
  (provider class `…PacketTunnelProvider`, type `com.apple.networkextension.packet-tunnel`).
- Entitlements on the app: `networkextension = packet-tunnel-provider-systemextension`,
  `system-extension.install`, `networking.multicast`.
- **Tunnel engine = an embedded Go library UniFi calls `unifi-tunnel`**, which is **sing-box (sagernet)
  + `sagernet/wireguard-go@v0.0.1-beta.7` + gVisor netstack** (userspace WireGuard). Confirmed by symbols:
  `unifi-tunnel/adapter/{inbound,outbound}`, `unifi-tunnel/transport/wireguard`,
  `unifi-tunnel/client/libutun/*`, sing-box fingerprints (`BoxService`, `Clash mode`,
  `CommandServer/CommandClient`, `SetSystemProxyEnabled`), and WireGuard config fields
  (`AllowedIPs`, `PresharedKey`, `PersistentKeepaliveInterval`, `reserved [3]uint8`).
- User-facing options: **Split Tunneling** ("Split VPN Is Active"), **Auto Reconnect**,
  **"Connect to VPN at UniFi Endpoint Startup"**, multi-**Site** switching
  ("All Sites", "Disconnect & Switch Site", "connect you to the best VPN site").
- WireGuard peer config is **fetched per-session from the UID API after login** (ephemeral), *not* a
  static `.conf` the user downloads. This is the key difference from a plain `wg-quick` setup.

### C. Auto-mount drives (UniFi Drive / UNAS)
- Class `NetFSMountCoordinator` (Swift) drives **SMB mounts** via `NetFS.framework`
  (`smb://`, `NetFSMount*`). Handles remount, force-unmount, dedup vs. existing Finder mounts
  ("same LAN IP and same shared name"), and `autoMountEnabled`.
- UI: "Auto Integrate Drive to Finder", "Mount All Drives", "%d drives", "Open Drive Portal",
  "Open UniFi Drive in Browser", "Successfully mounted %@ to Finder."
- Reachability states: **"Local Network Connected"**, **"Local Network Connected, %@ Not Mounted"**,
  **"Local Network Not Connected"** — mounting is driven by whether the drive is reachable.
- **Two reachability paths** (this is the important bit):
  1. **VPN-side / LAN path** — `FileAccessSMBViewModel._checkInVPNSideNetwork` mounts `smb://` over the
     LAN or over the One-Click VPN (normal L3 reachability).
  2. **CloudAccess relay path (remote, no VPN)** — the drive is addressed by **console/UNAS id**
     (`consoleStandardId`), resolved through `CloudAccessClient` / `CloudAccessUserTokenCoordinator` /
     `NcaSignalingTransport` / `SignalingChannel`, opening a **WebRTC peer connection** (data channels;
     "peer connection process signaling offer") with signaling via **`cloudaccess.svc.ui.com`**. SMB is
     tunneled over that relay. Same transport as UniFi remote screen-sharing.

### D. Admin / manage-site links
- "Manage sites and settings", "Admin Access", "Advanced management options like snapshot",
  "Open Drive Portal" → deep-links into `unifi.ui.com` / `account.ui.com/manage`.

### Bonus (in the app, not in scope for the port)
- **One-Click WiFi**: passwordless onboarding — "Instantly connect to WiFi without entering
  credentials" (802.1X/RADIUS creds provisioned via Identity), "Auto Connect to WiFi When Device Is Active".
- **MDM**: "This Mac is supervised and managed by: Identity MDM", "managed by your organization".

## 3. Implications for the Linux port

| Mac mechanism | Linux-port implication |
|---|---|
| Menu-bar `LSUIElement` agent | GNOME top-bar icon (needs AppIndicator/SNI — GNOME has no native tray) |
| Browser SSO + `identity-standard://` callback | `.desktop` `x-scheme-handler/identity-standard` + browser OAuth |
| WireGuard via sing-box/wireguard-go (userspace), per-session config from UID API | Reuse the **open-source** sing-box/wireguard-go stack, or NetworkManager/wg-quick; must implement the UID auth + config-fetch flow |
| SMB via NetFS `NetFSMountCoordinator`, per-drive, reachability-driven | GIO/GVfs `smb://` mounts (userspace, integrates with Files) or kernel cifs; selective per-drive automount, mount-when-reachable |
| **CloudAccess WebRTC relay** for remote (no-VPN) drive access | **Proprietary — out of scope.** Would require reversing UI's CloudAccess signaling/relay + auth. |
| Realm/Atlas + Firebase + AWS IoT | Only needed for push/remote-config parity; skippable for a minimal client |

### Scope decision (agreed with user)
- **In scope:** selective per-drive auto-mount that works **locally (LAN) and over the One-Click VPN**,
  reachability-driven mount/unmount.
- **Out of scope:** roaming remote drive access without a VPN (the CloudAccess WebRTC bridge) — proprietary.

## 4. Endpoints seen in the binary (reference)
```
account.ui.com  account.ui.com/manage      sso.ui.com (+ stg/dev)
api-gw.uid.df.ui.com (+ alpha)             enterprise.svc.ui.com (+ stg/dev)
cloudaccess.svc.ui.com (+ stg/dev)         unifi.ui.com
config.ubnt.com  fw-update.ubnt.com        feedback.svc.ui.com
help.ui.com/.../Troubleshooting-Identity-One-Click-VPN
```
