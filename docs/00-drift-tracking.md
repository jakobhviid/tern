# Drift Tracking — keeping tabs on the macOS client

Ubiquiti ships no changelog for the endpoint internals, so we detect change ourselves by
**fingerprinting each release and diffing**. When our Linux port suddenly breaks, the first question
is always "what did the Mac app change?" — this folder answers it.

## How it works

`scripts/fingerprint-macos-app.sh` is a **read-only** probe of `/Applications/UniFi Endpoint.app`. It
emits a normalized, sorted, diff-friendly summary of everything we care about:

- version / bundle id / min-OS / SDK / URL schemes / Bonjour services
- code-signing identity (Team ID `4P645293E8` = Ubiquiti)
- entitlements (sorted keys)
- linked **system** frameworks + **bundled** frameworks
- system extensions (+ Network-Extension provider class) and XPC services
- **presence checks** for the tunnel/drive/cloud engine markers (wireguard-go, sing-box, `unifi-tunnel/`,
  gVisor netstack, `NetFSMountCoordinator`, `FileAccessSMBViewModel`, `CloudAccessClient`, …)
- all `*.ui.com` / `*.ubnt.com` endpoints
- a curated set of user-facing feature strings from `en.lproj`
- SHA-256 of `Info.plist` + `Localizable.strings`, and the main-binary size

## Refresh procedure (after every app update)

```sh
# 1. capture a new fingerprint, named by date + version
VER=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" \
      "/Applications/UniFi Endpoint.app/Contents/Info.plist")
scripts/fingerprint-macos-app.sh > "docs/fingerprints/$(date +%F)-v${VER}.txt"

# 2. diff against the previous baseline
diff docs/fingerprints/<previous>.txt docs/fingerprints/$(date +%F)-v${VER}.txt

# 3. record what changed in the version log below, and open follow-ups for anything
#    that affects the port (new endpoint host, tunnel-engine swap, new entitlement, …)
```

## What a diff is telling you

| If the diff shows… | It probably means… | Port impact |
|---|---|---|
| New/removed **endpoint host** | API surface moved (new region, new service) | Update base URLs in the auth/broker client |
| `sagernet/*` or `unifi-tunnel/` marker **flips to ABSENT** | They swapped the tunnel engine | Re-validate the WireGuard handshake assumptions |
| New **entitlement** (esp. networking/system-extension) | New OS capability in use | Check whether it implies a new transport/feature |
| Changed **system-extension provider class** | VPN packet-tunnel internals reworked | Low direct impact (we don't use their sysext) but signals a rewrite |
| New/renamed **feature strings** (VPN/drive) | New user-facing behavior or setting | Consider mirroring it in the Linux UI |
| `FileAccessSMBViewModel` / `CloudAccessClient` **changes** | Remote-drive bridge reworked | Confirms / re-scopes the out-of-scope relay path |
| Only **hashes + binary size** change | Routine rebuild, no structural change | None |

## When the port breaks, checklist

1. Re-run the fingerprint and diff — did an **endpoint** or **auth** surface move?
2. Capture the live app's traffic while it logs in + connects (mitmproxy / Charles) and compare to
   `docs/02-vpn-protocol-and-reference-clients.md`. The auth/broker flow is the most likely thing to shift.
3. Check the two reference clients' repos for recent commits — the community usually spots protocol
   changes fast (`darki73/telepy-cli`, `sinnet3000/teleport-client`).

## Version log

| Date | Version (build) | SDK | Notable vs. previous |
|---|---|---|---|
| 2026-07-30 | **4.1.1 (177)** | macosx26.2 | **Baseline.** Engine: sing-box + sagernet/wireguard-go + gVisor. SMB via `NetFSMountCoordinator`; CloudAccess/WebRTC remote file path present. One-Click VPN + One-Click WiFi + UniFi Drive (on-demand placeholder files). |

> Add one row per release. Keep the raw fingerprints in `docs/fingerprints/`; this table is the human summary.
