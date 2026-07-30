# Live capture findings — the real control-plane API

_From a mitmproxy capture of the genuine macOS **UniFi Endpoint** app (v4.1.1/177) on 2026-07-30, done on the
owner's Mac. All IDs/tokens/creds and owner infra are **redacted** — only endpoint paths + JSON shapes are
recorded here (that's all a reimplementation needs). Raw capture stayed in the scratchpad, never the repo._

## What pins and what doesn't (important for anyone re-capturing)
- The **login / SSO** endpoint **cert-pins** → cannot be MITM'd (the app refuses the proxy cert; login fails).
- The **API endpoints** (`enterprise.svc.ui.com`, `cloudaccess.svc.ui.com`, `cognito-identity…amazonaws.com`)
  do **not** pin — they decrypted cleanly.
- **Pinning only blocks *interception*, not a real client.** Our Linux client talks to the real servers with
  real certs, so pinning is irrelevant to it. The remaining unknowns are discovered by *being a client and
  probing the real API with real auth* (curl / the Linux client), not by more MITM.

## The native-app control plane (captured, real shapes)

UA on these calls: `Identity Standard/4.1.1/177(...)` or `UniFi Endpoint/177 CFNetwork/...`. Auth model:
a **device-bound `Identity-Hub` JWT** (Bearer) → mints short-lived per-request user-tokens → `remote-credentials`.

1. **Bootstrap config**
   `GET https://config.ubnt.com/cloudAccessConfig.json`  → cacheable JSON (was 304).

2. **Org / IdP lookup** (`Client-Type: desktop-mac`, `Bearer` not required here)
   `GET https://enterprise.svc.ui.com/api/v2/organizations/domains/idps?domain=<ORG_SLUG>`
   ```jsonc
   {"data":{"orgId":"<ORG_UUID>","owner":"<USER_UUID>","name":"<fabric name>","subdomain":"<slug>",
     "idps":[{"name":"none","domain":"<ORG_UUID>.idp.ui.direct","protocol":"none"}],"idpProvider":"none"},
    "httpStatusCode":200}
   ```

3. **Mint a cloudaccess user-token** (`Bearer <IDENTITY_HUB_JWT>`, `x-user-id: <USER_UUID>`, `client-type: desktop-mac`, empty body)
   `POST https://cloudaccess.svc.ui.com/user-token/<DEVICE_ID>?withSessionToken=true`
   → `{"userTokenId":"<UUID>","expireAt":<epoch>}`
   - `<IDENTITY_HUB_JWT>` is **HS256**, payload: `{"iss":"Identity-Hub","exp","nbf","iat","jti","userId":"<USER_UUID>","type":"org","deviceId":"<DEVICE_ID>"}` — i.e. a **device-bound capability token**.
   - `<DEVICE_ID>` shape: `<32-hex-ish console id>:<number>` (identifies the target console/gateway).

4. **Mint a host user-token** (`Bearer <IDENTITY_HUB_JWT>`, empty body)
   `POST https://enterprise.svc.ui.com/api/v1/organizations/<ORG_UUID>/user-token/hosts/<DEVICE_ID>`
   → `{"data":{"userTokenId":"<UUID>","expireAtSec":<epoch>},"httpStatusCode":200}`

5. **Remote-access credentials — the key call** (`authorization: TEMP <userTokenId from step 4>`)
   `POST https://cloudaccess.svc.ui.com/ids/remote-credentials`  body `{"withTurn":true}`
   ```jsonc
   {"identityId":"us-west-2:<...>","accessKeyId":"ASIA<...>","secretKey":"<...>","sessionToken":"<...>",
    "expiration":<ms>,"region":"us-west-2","connectionState":"connected","deviceId":"<DEVICE_ID>",
    "userToken":"<JWT>","directAccessDomain":"<CONSOLE_HEX>.id.ui.direct",
    "turnCredentials":{"username":"<...>","password":"<...>","ttl":"86400",
      "uris":["stun:stun.cloudflare.com:3478","turn:turn.cloudflare.com:3478?transport=udp",
              "turn:turn.cloudflare.com:3478?transport=tcp","turns:turn.cloudflare.com:443?transport=tcp"]}}
   ```
   - Sets cookie `USER_TOKEN_SESSION=<uuid>`. Note the **`TEMP <token>`** auth scheme (not `Bearer`).

Separately, the **identity.ui.com web SPA** (Brave, `origin: chrome-extension://…`, AWS Amplify) uses **Cognito**:
`POST cognito-identity.eu-west-1.amazonaws.com` `GetId` then `GetCredentialsForIdentity` against Identity Pool
`eu-west-1:<POOL_ID>` + User Pool `eu-west-1_GRKlTYjgb` (app client `<aud>`), returning AWS temp creds. This is
the **web** path, distinct from the native app's Identity-Hub path.

## Data plane — inferred (NOT a plain NetworkManager config)
`remote-credentials` returns **Cloudflare TURN creds + a `directAccessDomain` (`<console>.id.ui.direct`)** — not a
WireGuard `.conf`. Combined with the binary (WebRTC + sing-box + wireguard-go + gVisor netstack) and the fact
that the **connect made no proxyable HTTP call**, the One-Click VPN data plane is:

> **ICE/WebRTC to the console** — directly via `directAccessDomain` when reachable, or **relayed via Cloudflare
> TURN** when NAT'd — with **WireGuard run in userspace** over that channel.

This is the **consumer-Teleport shape**, not a "fetch a config, hand to NetworkManager" flow. Confidence:
**HIGH** on the bootstrap (captured); **MEDIUM** on the exact WG-over-WebRTC data plane (inferred — the WG
provisioning rides the userspace/UDP channel to the console, which an HTTP proxy can't observe).

## Implications for the Linux client
- **Revises ADR-0004.** NetworkManager + a static WireGuard config is likely **insufficient** for the true
  One-Click flow. Realistic options, to decide on Bazzite:
  1. **Userspace WireGuard + ICE/TURN** (like `darki73/telepy-cli`, or boringtun + an ICE lib) — matches the app,
     works behind NAT, but is the most work and re-raises the sing-box(GPL) temptation (avoid; use permissive ICE).
  2. **Direct path for reachable consoles:** if `<console>.id.ui.direct` resolves to a reachable endpoint and the
     WG params can be obtained over the direct channel, a plain WireGuard tunnel (NetworkManager) may work for the
     owner's own console. **Probe this first** — it's the simplest viable path.
  3. **Console's built-in WireGuard Server** → plain `.conf` → NetworkManager. Pragmatic, but "not the product."
- **Auth for the client:** reproduce the **device-bound `Identity-Hub` JWT** → user-token → `remote-credentials`
  chain above. The one piece we could **not** capture (it's behind the pinned login) is **how the Identity-Hub
  JWT is first minted** from the invite/SSO. Discover it client-side (below).

## Still unknown → get it by client-side probing (pinning doesn't apply to us)
1. **Identity-Hub JWT mint:** how `code`(invite) or `UBIC_AUTH`(SSO cookie) → the device `Identity-Hub` JWT. Try:
   authenticate via `sso.ui.com/api/sso/v1/login` (password+TOTP → `UBIC_AUTH`, already validated), then probe the
   identity-hub/enrollment endpoints with that session to see the exchange. Look for a `credential/*` or
   `identity-hub` token endpoint.
2. **WireGuard params:** what the console returns over the direct/TURN channel to establish WireGuard.
3. Confirm whether `directAccessDomain` gives a directly-dialable WireGuard endpoint for the owner's console
   (decides option 2 vs 1 above).

## Confirmed reachable, non-pinned endpoints (safe reference)
`config.ubnt.com/cloudAccessConfig.json` · `enterprise.svc.ui.com/api/v2/organizations/domains/idps` ·
`enterprise.svc.ui.com/api/v1/organizations/<org>/user-token/hosts/<device>` ·
`cloudaccess.svc.ui.com/user-token/<device>` · `cloudaccess.svc.ui.com/ids/remote-credentials`
(auth `TEMP <userTokenId>`) · Cognito `eu-west-1` (web path).
