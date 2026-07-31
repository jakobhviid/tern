# Dependencies & Licensing Log

> Every component we might use, with its license + project link, plus packaging availability and the
> copyleft rules that constrain how we combine them. **Goal: ship our own code under MIT/Apache with zero
> copyleft obligation.** That is achievable — see the golden rules and recommended set below.
> Confidence: all license facts verified against the projects' own LICENSE files (HIGH) unless noted.
> Not legal advice — see the ToS/interop note at the end.

## Three facts that shape everything
1. **sing-box is GPL-3.0** (+ a name/endorsement restriction). In-process linking it makes our whole app
   GPLv3. **Avoid** — use its permissive building blocks instead.
2. **Samba / libsmbclient / cifs-utils are GPL-3.0-or-later** (not LGPL). This is the real copyleft chain to
   design around: keep SMB **out-of-process** (exec `mount.cifs`, or use GVfs).
3. **Both reference clients are MIT** — their SSO/handshake code is reusable with attribution.

## License table

| Component | Role | SPDX | Copyleft | Link |
|---|---|---|---|---|
| **sagernet/sing-box** | Tunnel engine (what UniFi embeds) | **GPL-3.0-or-later** + name clause | **Strong — avoid linking** | https://github.com/SagerNet/sing-box/blob/testing/LICENSE |
| SagerNet/wireguard-go | Userspace WireGuard (Go) | **MIT** | None | https://github.com/SagerNet/wireguard-go/blob/main/LICENSE |
| zx2c4 wireguard-go | Userspace WireGuard (Go) | **MIT** | None | https://git.zx2c4.com/wireguard-go/tree/LICENSE |
| **cloudflare/boringtun** | Userspace WireGuard (Rust) | **BSD-3-Clause** | None | https://github.com/cloudflare/boringtun/blob/master/LICENSE.md |
| google/gvisor (`pkg/tcpip`) | Userspace TCP/IP netstack | **Apache-2.0** (+patent grant) | None | https://github.com/google/gvisor/blob/master/LICENSE |
| wireguard-tools (`wg`,`wg-quick`) | CLI config | **GPL-2.0** (some files GPL-2.0 OR MIT) | Strong (as a whole) | https://git.zx2c4.com/wireguard-tools/tree/COPYING |
| NetworkManager (daemon) | VPN/link mgmt | **GPL-2.0-or-later** | Strong (daemon) | https://github.com/NetworkManager/NetworkManager |
| NetworkManager `libnm` | Client library | **LGPL-2.1-or-later** | Weak (dynamic-safe) | https://github.com/NetworkManager/NetworkManager |
| gtk4-rs (`gtk4`, `libadwaita` crates) | Rust GUI bindings | **MIT** | None | https://crates.io/crates/gtk4 · https://crates.io/crates/libadwaita |
| **libadwaita** (C) | GNOME widgets | **LGPL-2.1-or-later** | Weak (dynamic-safe) | https://gitlab.gnome.org/GNOME/libadwaita/-/raw/main/COPYING |
| **relm4** | Rust GUI framework | **Apache-2.0 OR MIT** | None | https://crates.io/crates/relm4 |
| Tauri v2 | Rust app framework | **Apache-2.0 OR MIT** | None | https://crates.io/crates/tauri |
| Iced | Rust GUI framework | **MIT** | None | https://crates.io/crates/iced |
| **Slint** | Rust GUI framework | **GPL-3.0-only OR royalty-free-w/-attribution OR paid** | **Tri-license — avoid** | https://github.com/slint-ui/slint/blob/master/LICENSE.md |
| **ksni** | Tray / StatusNotifierItem | **Unlicense** (public domain) | None | https://crates.io/crates/ksni |
| glib / gio (C) | Base GNOME libs | **LGPL-2.1-or-later** | Weak (dynamic-safe) | https://github.com/GNOME/glib |
| **Samba / libsmbclient** | SMB client lib | **GPL-3.0-or-later** | **Strong — keep out-of-process** | https://github.com/samba-team/samba |
| **cifs-utils** (`mount.cifs`) | Kernel SMB mount helper | **GPL-3.0-or-later** | Strong (exec, don't link) | https://git.samba.org/?p=cifs-utils.git |
| util-linux (`mount`) | Generic mount | **GPL-2.0-or-later** (libs LGPL-2.1) | Strong (CLI) | https://github.com/util-linux/util-linux/blob/master/README.licensing |
| GVfs (+ SMB backend) | GNOME VFS | **LGPL-2.0+** src; SMB backend links GPL-3 libsmbclient | Use over portal (out-of-proc) | https://gitlab.gnome.org/GNOME/gvfs |
| libsecret | Keyring/secrets | **LGPL-2.1-or-later** | Weak (dynamic-safe) | https://github.com/GNOME/libsecret |
| oo7 | Rust secrets (libsecret/portal) | **MIT** | None | https://crates.io/crates/oo7 |
| zbus | Rust D-Bus | **MIT** | None | https://crates.io/crates/zbus |
| notify-rust | Notifications | **MIT OR Apache-2.0** | None | https://crates.io/crates/notify-rust |
| ashpd | XDG portals (Rust) | **MIT** | None | https://crates.io/crates/ashpd |
| **darki73/telepy-cli** | Reference client (Python) | **MIT** | None | https://github.com/darki73/telepy-cli/blob/master/LICENSE |
| **sinnet3000/teleport-client** | Reference client (Go) | **MIT** | None | https://github.com/sinnet3000/teleport-client/blob/main/LICENSE |

## GPL-contagion rules (the ones that bite)
- **In-process linking GPL = whole app is GPL.** FSF makes no static/dynamic distinction
  (gnu.org/licenses/gpl-faq #GPLStaticVsDynamic). So never link **sing-box** or **libsmbclient** into our binary.
- **LGPL via dynamic linking is safe** for a differently-licensed app, because the user can swap the system
  `.so` (LGPLv2.1 §6). gtk4/libadwaita/glib/libsecret/libnm dynamically linked → **our code stays permissive.**
- **Exec-as-subprocess = mere aggregation, not a derivative work** (gnu.org FAQ #MereAggregation), as long as it's
  a real process boundary over pipes/sockets/CLI/D-Bus. So we may freely `exec` `wg`/`wg-quick`/`nmcli`/`gio`/
  `mount.cifs` — even sing-box — without copyleft on our own code. (This linking-vs-derivative line is FSF's
  own aggressive reading and not fully court-settled; flagged as the single legal-theory caveat.)
- **Slint** is the only GUI option that can force GPLv3, mandate attribution, or require payment — don't use it.

## Recommended LICENSE-CLEAN set (keeps our app MIT/Apache)

| Layer | Pick | License |
|---|---|---|
| Userspace WireGuard (if not using NM/kernel) | **boringtun** (BSD-3) or **wireguard-go** (MIT); or exec `wg-quick` | Permissive / arm's-length |
| Userspace TCP/IP (only if needed) | **gVisor `pkg/tcpip`** (Apache-2.0) | Permissive |
| VPN mgmt (preferred) | **NetworkManager** via `zbus`/`libnm` (dynamic) or exec `nmcli` | Weak/none |
| Tunnel orchestration | **roll our own** (reuse the MIT reference clients) | Ours |
| GUI | **relm4** (Apache/MIT) on gtk4-rs/libadwaita, dynamically linked; or **iced** (MIT) | Permissive |
| Tray | **ksni** (Unlicense) | Public domain |
| Secrets | **oo7** + libsecret (dynamic) | MIT / weak |
| SMB | exec **`gio`/GVfs** (portal) or **`mount.cifs`** subprocess | None on our code |

**Golden rules:** (1) never in-process-link sing-box or libsmbclient; (2) dynamically link the LGPL GNOME
stack; (3) prefer relm4/iced over Slint. → app is cleanly MIT/Apache-licensable and packageable everywhere.

## In use: the Teleport data-plane crates (ADR-0016)

What the built data plane actually links, and how each stays inside the `deny.toml` allow-list:

| Crate | Role | License | Note |
|---|---|---|---|
| **boringtun 0.7** | Userspace WireGuard (Noise + transport) | **BSD-3-Clause** | 0.6 hard-pins an rc `x25519-dalek`; 0.7 is clean. |
| **ring 0.17** | Crypto primitives (pulled by boringtun 0.7) | **Apache-2.0 AND ISC** | *Not new, not OpenSSL:* our rustls stack already links it as its provider. The OpenSSL/native-tls **ban** targets TLS backends; ring is Apache/ISC and allowed. |
| **tun-rs 2.x** | TUN device (`async_tokio`) | **MIT / Apache-2.0** | Chosen over the `tun` crate, which is **WTFPL** (not on the allow-list). |
| ip_network, ip_network_table | boringtun deps | **BSD-2-Clause** | Added `BSD-2-Clause` to the allow-list — same permissive family as the already-allowed BSD-1/3. |

Net: no GPL/copyleft, no OpenSSL/native-tls — `cargo deny check licenses bans sources` stays green. The
allow-list gained only `BSD-2-Clause`; the bans list is unchanged.

## Packaging availability (summary)
- **Homebrew (macOS + Linuxbrew):** `sing-box`, `wireguard-go`, `wireguard-tools`, `boringtun`, `samba`,
  `libsecret`, `gtk4`, `libadwaita` all in core with bottles. **Gap: `cifs-utils`** (Linux-kernel helper; get
  from distro, not brew). Publishing *our* GUI: Linux GUI is **not** a good brew fit (casks are macOS-only);
  use a **first-party tap** for CLI bits, and Flatpak/distro for the Linux GUI. macOS build → cask/tap.
- **Flatpak (GNOME runtime `org.gnome.Platform`):** gtk4, libadwaita, glib/gio, libsecret (via Secret portal)
  come free from the runtime. **Samba/libsmbclient is NOT in the runtime** → reach SMB via **GVfs over the
  portal** (keeps GPL-3 out-of-process) rather than bundling libsmbclient. Bundling GPL/LGPL in a Flatpak is
  fine (redistribution is permitted) as long as source + notices ship (Flathub's 2025 license tooling handles
  this) — but bundling libsmbclient into our Flatpak would pull GPLv3 onto the app, so don't.
- **Native (.deb/.rpm/AUR):** GUI/mount/secret stack (gtk4, libadwaita, glib, libsecret, samba, cifs-utils,
  gvfs+`gvfs-smb`, NetworkManager, wireguard-tools) are plain dependency declarations everywhere. **Vendor
  flags:** sing-box (Deb+Fedora — but we're avoiding it), boringtun (Cargo crate, trivial), wireguard-go
  (Deb/Fedora likely absent), gVisor (Fedora unreliable). Rust GUI crates are Cargo-vendored as normal.

## Reference-client reuse
Both `darki73/telepy-cli` and `sinnet3000/teleport-client` are **MIT** (verified 2026-07-30). We may read,
copy code into an MIT/Apache/BSD project, and redistribute — the only obligation is preserving the MIT notice.
Re-verify if either repo changes its LICENSE. Note: they implement **consumer Teleport**, which (per doc 02
UPDATE) is a *different* endpoint family than the desktop UCS VPN — so their **SSO/MFA login** code
(`/api/sso/v1/...`) is directly reusable, but the **VPN-provisioning** half must be adapted to the UCS API.

## ToS / interoperability — NOT legal advice
Component licenses say nothing about Ubiquiti's **Terms of Service** governing the endpoints we call — that's a
separate contract layer. Clean-room reimplementation for interoperability is an established practice (e.g. EU
Software Directive Art. 6) but is fact-specific and unsettled in places. Watch for: anti-circumvention/DMCA if
any auth step is a technical protection measure; "no automated access"/anti-RE clauses in the vendor ToS;
**trademark** — don't use "UniFi"/"Ubiquiti" in the app name/branding. Posture: clean-room from the MIT
references + our own observation, no vendor code/marks/assets, and get counsel review before public release.

## Bottom line
Ubiquiti chose GPLv3 sing-box for convenience; we don't need to. Its underlying primitives
(wireguard-go/MIT, boringtun/BSD, gVisor/Apache) give the identical capability with zero copyleft. Build the
orchestration ourselves (reusing the MIT reference clients), render with relm4/iced over the dynamically-linked
LGPL GNOME stack, keep the two GPL-3 hazards (sing-box, libsmbclient) out-of-process → **the app can ship MIT/
Apache, on Flatpak + native packages, cleanly.**
