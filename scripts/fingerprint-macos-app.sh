#!/usr/bin/env bash
#
# fingerprint-macos-app.sh — read-only, diffable fingerprint of "UniFi Endpoint.app".
#
# Purpose: detect DRIFT in Ubiquiti's macOS client over time. Re-run after each app
# update; commit the output under docs/fingerprints/. `git diff` between fingerprints
# shows exactly what changed (new entitlements, added/removed frameworks, tunnel-engine
# swaps, new endpoints, renamed feature strings) so we can keep the Linux port in sync
# and fix it when Ubiquiti changes something.
#
# Usage:
#   scripts/fingerprint-macos-app.sh [ /path/to/UniFi\ Endpoint.app ] > docs/fingerprints/$(date +%F).txt
#
# Read-only: never writes to or modifies the app bundle.

set -u
APP="${1:-/Applications/UniFi Endpoint.app}"
BIN="$APP/Contents/MacOS/UniFi Endpoint"
PLIST="$APP/Contents/Info.plist"

if [[ ! -d "$APP" ]]; then echo "ERROR: app not found at: $APP" >&2; exit 1; fi

pb() { /usr/libexec/PlistBuddy -c "Print :$1" "$PLIST" 2>/dev/null; }
rule() { printf '\n===== %s =====\n' "$1"; }

# Cache the (large) strings dump once.
STRINGS_CACHE="$(mktemp -t uendpoint-strings)"
trap 'rm -f "$STRINGS_CACHE"' EXIT
strings -a "$BIN" 2>/dev/null > "$STRINGS_CACHE"

has() { grep -qaF -- "$1" "$STRINGS_CACHE" && echo "present" || echo "ABSENT "; }

echo   "UniFi Endpoint — fingerprint"
echo   "generated: $(date -u +%FT%TZ) (UTC)"
echo   "app path:  $APP"

rule "VERSION / IDENTITY"
printf 'CFBundleShortVersionString : %s\n' "$(pb CFBundleShortVersionString)"
printf 'CFBundleVersion            : %s\n' "$(pb CFBundleVersion)"
printf 'CFBundleIdentifier         : %s\n' "$(pb CFBundleIdentifier)"
printf 'LSMinimumSystemVersion     : %s\n' "$(pb LSMinimumSystemVersion)"
printf 'DTSDKName                  : %s\n' "$(pb DTSDKName)"
printf 'LSUIElement                : %s\n' "$(pb LSUIElement)"
printf 'URL schemes                : %s\n' "$(pb 'CFBundleURLTypes:0:CFBundleURLSchemes:0')"
printf 'Bonjour services           : %s %s\n' "$(pb 'NSBonjourServices:0')" "$(pb 'NSBonjourServices:1')"

rule "CODE SIGNATURE"
codesign -dvv "$APP" 2>&1 | grep -E "^(Authority|TeamIdentifier|Identifier|Format)=" || echo "(codesign unavailable)"

rule "ENTITLEMENTS (sorted keys)"
codesign -d --entitlements :- "$APP" 2>/dev/null | tr -d '\0' \
  | grep -oE '<key>[^<]+</key>' | sed 's/<[^>]*>//g' | sort -u || echo "(none)"

rule "LINKED SYSTEM FRAMEWORKS (names only, sorted)"
otool -L "$BIN" 2>/dev/null | grep -oE '/System/Library/Frameworks/[A-Za-z0-9]+\.framework' \
  | sort -u

rule "BUNDLED FRAMEWORKS (sorted)"
ls "$APP/Contents/Frameworks" 2>/dev/null | sort

rule "SYSTEM EXTENSIONS"
find "$APP/Contents/Library/SystemExtensions" -maxdepth 1 -name '*.systemextension' 2>/dev/null \
  -exec basename {} \; | sort
for sx in "$APP"/Contents/Library/SystemExtensions/*.systemextension; do
  [[ -d "$sx" ]] || continue
  /usr/libexec/PlistBuddy -c "Print :NetworkExtension:NEProviderClasses" "$sx/Contents/Info.plist" 2>/dev/null \
    | grep -E '=' | sed 's/^ */  provider: /'
done

rule "XPC SERVICES"
ls "$APP/Contents/XPCServices" 2>/dev/null | sort

rule "TUNNEL / DRIVE / CLOUD ENGINE MARKERS (presence)"
printf '  %-42s %s\n' "sagernet/wireguard-go"        "$(has 'sagernet/wireguard-go')"
printf '  %-42s %s\n' "sagernet/sing (sing-box)"      "$(has 'sagernet/sing')"
printf '  %-42s %s\n' "unifi-tunnel/ (UI Go wrapper)" "$(has 'unifi-tunnel/')"
printf '  %-42s %s\n' "sing-box BoxService"           "$(has 'BoxService')"
printf '  %-42s %s\n' "gVisor netstack (pkg/tcpip)"   "$(has 'pkg/tcpip')"
printf '  %-42s %s\n' "WireGuardOutboundOptions"      "$(has 'WireGuardOutboundOptions')"
printf '  %-42s %s\n' "NetFSMountCoordinator"         "$(has 'NetFSMountCoordinator')"
printf '  %-42s %s\n' "FileAccessSMBViewModel"        "$(has 'FileAccessSMBViewModel')"
printf '  %-42s %s\n' "CloudAccessClient"             "$(has 'CloudAccessClient')"
printf '  %-42s %s\n' "NcaSignalingTransport (WebRTC)" "$(has 'NcaSignalingTransport')"
printf '  %-42s %s\n' "smb:// scheme"                 "$(has 'smb://')"

rule "ENDPOINTS (ui.com / ubnt.com hosts, sorted unique)"
grep -aoE 'https://[a-zA-Z0-9._-]*(ui\.com|ubnt\.com)[a-zA-Z0-9._/-]*' "$STRINGS_CACHE" \
  | sort -u

rule "API PATHS (auth / ucs-vpn / identity / credential, sorted unique)"
grep -aoE '/(api/sso|api/oauth|api/auth|proxy/ucs|proxy/users|user-token|sso/identity|standard/api|api/v[12]/credential|api/v[12]/identity)[a-zA-Z0-9/_.:{}?=-]*' "$STRINGS_CACHE" \
  | sort -u

rule "FEATURE STRINGS (en.lproj, curated, sorted unique)"
EN="$APP/Contents/Resources/en.lproj"
for f in "$EN"/Localizable.strings "$EN"/MainMenu.strings; do
  [[ -f "$f" ]] && plutil -convert xml1 -o - "$f" 2>/dev/null
done | grep -oE '<string>[^<]{3,90}</string>' | sed 's/<[^>]*>//g' \
  | grep -iE 'vpn|drive|mount|admin|manage|teleport|split|tunnel|site|one-click|auto ?connect|sign ?in|sign ?out|reauth|finder|share|portal' \
  | sort -u

rule "RESOURCE HASHES (change signal)"
for f in "$PLIST" "$EN/Localizable.strings"; do
  [[ -f "$f" ]] && printf '  %s  %s\n' "$(shasum -a 256 "$f" | cut -d' ' -f1)" "${f#$APP/}"
done
printf '  main binary size (bytes)   : %s\n' "$(stat -f%z "$BIN" 2>/dev/null)"

echo
echo "===== END ====="
