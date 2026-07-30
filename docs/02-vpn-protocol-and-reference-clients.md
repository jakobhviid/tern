# One-Click VPN — Protocol, Provisioning & Reference Clients

> Combines the macOS binary teardown (doc 01) with web research + inspection of existing open-source
> reverse-engineered clients. Confidence tags: **High** = official docs or code-verified; **Medium** =
> consistent community evidence; **Low** = single/unverified source.

## TL;DR
There is **no static WireGuard `.conf` to export** from UniFi Identity's One-Click VPN — the tunnel is
cloud-provisioned, key-rotated, and device-managed. A Linux client must **re-implement the cloud
handshake**, then run **userspace WireGuard**. Two existing open-source clients do this for consumer
**Teleport** — but see the **UPDATE** below: static analysis of the desktop binary shows the UID desktop
client actually uses a **different, cleaner API (the "UCS" `vpn/session` endpoint)**, not the Teleport broker.

## Provisioning model — **High**
- **Invitation/enrollment based, admin-driven, per-user.** Admin enables the service + assigns
  people/permissions; user is onboarded by an **invite email** (optional required 2FA code, ~30-day
  expiry). The cloud **generates the WireGuard keypair + peer config and pushes it to the app** — the
  user never sees a config file.
- **~2 peers per identity** — one "Desktop", one "Mobile" (community-reported; two same-type devices
  share an IP and can't both connect at once). **[Medium]**
- **Full-tunnel by default; split-tunnel is admin-configured** server-side (client just receives routes).
- **DNS** is forced through the tunnel (`SupplementalMatchDomains=[""]` on macOS). MTU **1420**.
- Runs over **UDP 51820**; requires the console be publicly reachable (port-forward / cloud relay).

## Consumer Teleport flow (from `darki73/telepy-cli`, observed against live service) — **High** for Teleport
> ⚠️ This is the **consumer Teleport** flow. The **desktop UID client uses a different API** — see the UPDATE
> section below, which is the one we must implement. This section is kept for the reusable SSO/MFA patterns.
1. **SSO login:** `POST https://sso.ui.com/api/sso/v1/login {user,password}`
   → `499 UBIC_2FA` challenge → resubmit `{user,password,token}` (6-digit TOTP) → `200 UBIC_AUTH`;
   push-MFA poll at `.../api/sso/v1/user/self/mfa/push/poll-login`. Yields a trusted-session cookie.
2. **Console/VPN directory:** `GET https://cloudaccess.svc.ui.com/network-cloud/v2/sd-wan/hosts?withVpnConf=true`
   (AWS **SigV4**, service `execute-api`) → hosts + `wanIp` + routable LAN subnets + VPN conf.
3. **Teleport broker/signaling:** `POST {base}/teleport {token, payload:{request_type,...}}` →
   `{teleportRequestId}`, then poll `GET {base}/teleport/{reqId}` (SigV4 / API-Gateway-IAM).
   Signaling adapters seen: cloud HTTP, **MQTT**, **UCP4**, and a native **ICE** (STUN NAT-traversal)
   adapter — matches the WebRTC/`NcaSignalingTransport` seen in the macOS binary.
4. **Data plane:** **userspace WireGuard + userspace TCP/IP stack** (matches macOS = wireguard-go + gVisor
   netstack). `sinnet3000/teleport-client` confirms base `https://cloudaccess.svc.ui.com/teleport`,
   authenticated STUN nomination, in-process WireGuard, exposes a local SOCKS5.

## UPDATE (2026-07-30) — RESOLVED: the desktop UID client uses its own "UCS" API, not the Teleport broker

Static extraction of API paths from the macOS binary settles the earlier uncertainty: the desktop **UID
One-Click VPN does NOT use the consumer-Teleport broker** (`cloudaccess.svc.ui.com/network-cloud/v2/sd-wan/
hosts` + `/teleport` + ICE/STUN). It uses a distinct, REST-style flow through a `/proxy/ucs/...` gateway.
**This is the flow we must implement.** Confidence: paths + field names are HIGH (present in the binary);
exact request/response shapes are MEDIUM (inferred — a one-session capture would confirm, but the field
strings make the shape clear).

**Desktop UID flow (the target):**
1. **SSO** (same family the reference clients use — reusable): `POST /api/sso/v1/login/start` → `/login` →
   `/login/2fa` → poll `/login/token/poll`; push-MFA `/user/self/mfa/push/poll-login`; also
   `/sso/identity_providers` + `/sso/identity_provider/url` for **enterprise SAML/external IdP**. On
   `sso.ui.com`. Yields a session/JWT. (The Mac app runs this in the **system browser**, which transparently
   handles MFA + SAML — we mirror that; see docs 03/05.)
2. **Host directory:** `GET /user-token/hosts/…` — the consoles/sites available to the user (addressed by
   `consoleStandardId` / `consoleId`, not IP).
3. **Device enrollment:** client generates a WireGuard keypair and uploads its **public key** —
   `/proxy/users/public/api/v2/identity/public_key` (+ `/identity/info`); credential endpoints
   `/api/v2/credential/{device,confirm,download}`, `/api/v1/credential/private-key`.
4. **VPN session (the core):** `POST /proxy/ucs/public/user/api/v1/vpn/session` → returns a **standard
   WireGuard peer config**. Field names present in the binary: `wgConfig`, `serverPublicKey`/`server_public_key`,
   `endpoint`, `listenPort`, `allowedIps`/`allowed_ips`, `presharedKey`/`preshared_key`, `persistentKeepalive`,
   the client `privateKey`/`publicKey`, and `sessionId`. Status/heartbeat: `/vpn/session/status`. Bearer-token
   auth (the SSO JWT), via the UID API gateway (`api-gw.uid.df.ui.com` / `enterprise.svc.ui.com`).
5. **Bring up WireGuard** with that config (NetworkManager / kernel wg). **No ICE/STUN needed** when the
   gateway has a reachable public `endpoint` — which One-Click VPN requires anyway (public console / port
   51820). The ICE/STUN/WebRTC machinery (`cloudaccess`, `NcaSignalingTransport`) is for the **remote-access +
   drive relay** path and NAT-punching consoles without a public endpoint — **out of scope** (doc 01).

**Why this is good news:** provisioning is a clean "get a WireGuard config from an endpoint" call, arguably
*simpler* than Teleport's ICE/STUN dance. The reference clients stay directly reusable for **SSO/MFA** (same
`/api/sso/v1/...` paths) and userspace-WireGuard technique; only the `ucs/vpn/session` request/response must be
adapted from consumer-Teleport to the UCS API.

**Remaining validation (nice-to-have, not a blocker):** one traffic capture to confirm the exact JSON shapes of
`identity/public_key` and `vpn/session`. The architecture is already clear enough to design against. **[Medium]**

## Reference clients (unofficial, for study/reuse)
| Repo | Lang | Value |
|---|---|---|
| `darki73/telepy-cli` | Python | Most complete: full SSO+MFA, SigV4 hosts call, teleport broker, userspace WG. Near-drop-in Linux client. |
| `sinnet3000/teleport-client` | Go | Confirms `cloudaccess.svc.ui.com/teleport`, STUN nomination, in-process WG, local SOCKS5, session persist. |
| `snoack/teleport-mtu-fix` | Shell | MTU 1420, `wg*` iface bring-up, `--lan-only` split mode. |
| `n-eiling/unifi-split-dns` | macOS | **Authoritative "cannot export UID config" statement**; DNS-forcing behavior; alternatives. |
| `willie5588912/unifi-teleport-router` | — | Gateway-side systemd route manager; `tlprtX` ifaces. |

Repo URLs: github.com/{darki73/telepy-cli, sinnet3000/teleport-client, snoack/teleport-mtu-fix,
n-eiling/unifi-split-dns, willie5588912/unifi-teleport-router}

## Officially-supported Linux fallback — **High**
UniFi Network **Settings → VPN → VPN Server → WireGuard** is a *separate, non-Identity* feature that DOES
let you **download a `.conf` / scan a QR** → usable directly with `wg-quick`/`wg` on Linux. If the goal is
only "a Linux box reaches the LAN," this is the supported route and sidesteps the whole cloud handshake —
at the cost of not being "the same One-Click experience" and being provisioned manually per device.

Docs: help.ui.com/hc/en-us/articles/115005445768-UniFi-Gateway-WireGuard-VPN-Server

## Drives — reconciling binary vs. official docs
- **Official:** UNAS/UniFi Drive shares mount over **SMB or NFS** and are reachable "when connected to the
  **local network or via VPN**." Ubiquiti documents **Linux mounting via NFS** (article 26277250895895).
  No official doc describes SMB traversing the cloud relay. **[High]**
- **Binary evidence (doc 01):** the desktop app clearly has a **CloudAccess relay path for file access**
  (`FileAccessSMBViewModel._checkInVPNSideNetwork`, `consoleStandardId`, `NcaSignalingTransport`,
  WebRTC signaling). So a **remote, no-VPN** desktop file path very likely exists but is **undocumented
  and proprietary**. Both interpretations agree on the practical plan below.
- **Plan for the port:** mount SMB/NFS over the tunnel (or LAN). Reproducing the no-VPN CloudAccess bridge
  is out of scope. Credentials are separate **"File Service Credentials"** (or AD), set per user.

Drive docs: help.ui.com/hc/en-us/articles/{39670142044567, 14276882157975, 26277250895895}
