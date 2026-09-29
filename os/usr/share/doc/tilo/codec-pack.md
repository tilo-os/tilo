# Additional codecs pack

The Tilo image ships no patented codecs. Its Mesa is Fedora's, built with
H.264/HEVC disabled: VA-API offers AV1/VP9 decode (and AV1 encode on AMD), but
no H.264 or HEVC. GStreamer is Tilo's only media stack, and without the pack it
has no H.264/HEVC encoder or decoder beyond what the stock libav build allows.

The additional codecs pack is opt-in. The owner chooses it explicitly, in one
of three places, none of which is selected by default:

* first-login setup, which offers "Install media codecs" with a short
  explanation of what it enables and where the packages come from; setup
  continues while the download runs in the background;
* the installer's Options step, which records the choice for the installed
  system (`tilo-install --codecs`, see "Installer handoff");
* Settings (Updates, "Additional codecs"), which also shows the status and
  offers Remove, Try again and a re-check for the Mesa part.

The packages are then downloaded on the device, directly from the third
parties that publish them, under their licences. Tilo does not host, mirror or
redistribute them.

| Source | Packages | Signed by |
| --- | --- | --- |
| RPM Fusion free | `gstreamer1-plugins-bad-freeworld`, `gstreamer1-plugins-ugly`, `libavcodec-freeworld` (used by the stock `gstreamer1-plugin-libav`), `mesa-va-drivers-freeworld`, and their dependencies (x264, x265, ...) | RPM Fusion free (2020) key |
| RPM Fusion nonfree | `intel-media-driver`, and nothing else (`includepkgs`) | RPM Fusion nonfree (2020) key |
| Cisco, through Fedora's `fedora-cisco-openh264` repository | `openh264`; binaries served by Cisco, metadata by Fedora | Fedora 44 primary key |
| Fedora | `gstreamer1-plugin-openh264` and dependencies the base lacks | Fedora 44 primary key |

Fedora 44 no longer has `mesa-vdpau-drivers-freeworld` (Mesa dropped VDPAU).
RPM Fusion also publishes `mesa-vulkan-drivers-freeworld` (Vulkan Video
H.264/HEVC); the pack does not include it, because it would replace the whole
Vulkan driver set rather than add VA-API entry points.

## Pinned keys

The helper imports keys into an isolated rpm keyring that holds nothing else,
checks each key's primary fingerprint against a value pinned in
`/usr/libexec/tilo-codec-pack` first, and then requires every downloaded RPM to
carry a valid signature from the key of the repository it came from.

| Key | File | Fingerprint |
| --- | --- | --- |
| RPM Fusion free for Fedora (2020) | `/usr/share/tilo/codec-pack/keys/RPM-GPG-KEY-rpmfusion-free-fedora-2020` | `E9A4 91A3 DE24 7814 E7E0 67EA E06F 8ECD D651 FF2E` |
| RPM Fusion nonfree for Fedora (2020) | `/usr/share/tilo/codec-pack/keys/RPM-GPG-KEY-rpmfusion-nonfree-fedora-2020` | `79BD B88F 9BBF 7391 0FD4 095B 6A2A F961 9484 3C65` |
| Fedora 44 primary | `/etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-primary` (from the base) | `36F6 12DC F27F 7D1A 48A8 35E4 DBFC F71C 6D9F 90A6` |

The RPM Fusion keys were fetched from <https://rpmfusion.org/keys>; their
fingerprints match the ones published there and the keys that
`rpmfusion-{free,nonfree}-release-44` install. RPM Fusion signs Fedora 32 and
later with these keys. The Fedora key is pinned per release: a base on a Fedora
release without a pinned key cannot get the pack until the image ships one.

## What goes where

Two directory system extensions under `/var/lib/extensions/`, each with
`usr/lib/extension-release.d/extension-release.<name>` (`ID=fedora`,
`VERSION_ID=<base>`, `ARCHITECTURE=x86-64`, `SYSEXT_SCOPE=system`, plus
`TILO_*` fields recording what it was built for):

* `tilo-codecs`: the GStreamer plugins, `libavcodec-freeworld`, OpenH264,
  `intel-media-driver`.
* `tilo-codecs-mesa`: `mesa-va-drivers-freeworld`. Kept separate so a Mesa
  mismatch can only switch hardware H.264/HEVC off, never break graphics.

Only `/usr` from each RPM is used; RPM scriptlets never run. RPM Fusion's
freeworld builds put a same-soname library (`libgallium-<version>.so`,
`libavcodec.so.*`) into a private directory and give it precedence with a file
in `/etc/ld.so.conf.d`. An extension cannot change `/etc`, so the helper moves
those libraries to the base's own path in `/usr/lib64`, where the extension
shadows the base's copy: the same effect, with no change to `/etc` or
`ld.so.cache`. The VA drivers stay in `/usr/lib64/dri-freeworld`, which
Fedora's libva searches before `/usr/lib64/dri`. `intel-media-driver`'s
`iHD_drv_video.so` shadows the base's codec-less `libva-intel-media-driver`
when that is installed.

Nothing in the pack may replace a package the base has installed: every base
package name is excluded from every repository while resolving, so dependencies
must be satisfied by the base as it is. Each extension carries a manifest at
`/usr/share/tilo-codec-pack/<name>.json` naming every package, its repository,
licence and signing key; the packages' licence texts come along in
`/usr/share/licenses`.

## Version lockstep

The freeworld VA driver brings its own `libgallium-<version>.so`, which then
serves every Mesa client in the process. It must be built from exactly the Mesa
version the base runs (`rpm -q mesa-dri-drivers`; releases may differ, the
upstream version may not). The GStreamer plugins follow the base's `gstreamer1`
and `libavcodec-freeworld` follows the base's `libavcodec-free`: the same
version if RPM Fusion has it, otherwise the newest older build of the same
major.minor. OpenH264 and Intel's driver have no base counterpart.

RPM Fusion's public mirrors keep only the release build and the latest update
of each package. If neither matches the base Mesa, `tilo-codecs-mesa` stays
disabled, the stock drivers stay in use and Settings shows "Hardware H.264/HEVC
is waiting for a matching Mesa build from RPM Fusion". It is retried at every
update check, after every boot and daily.

## Lifecycle

| Unit | When | What |
| --- | --- | --- |
| `tilo-codec-pack@install.service` | First-login setup or Settings, Install / Try again | Records the opt-in (`/var/lib/tilo/codec-pack/enabled`), then resolves, downloads, verifies, builds and merges |
| `tilo-codec-pack@remove.service` | Settings, Remove / Cancel installation | Removes the opt-in, both extensions, parked copies and the download cache, and unmerges |
| `tilo-codec-pack@refresh.service` | `tilo-codec-pack-refresh.timer` (5 min after boot, daily), every update check, NetworkManager reaching the internet, and Settings' re-check | Adopts an installer seed, then re-resolves for the booted base; a no-op without the opt-in |
| `tilo-codec-pack-guard.service` | Every boot, offline, before `systemd-sysext.service` | Sets aside a `tilo-codecs-mesa` whose recorded Mesa version differs from the booted base, or restores a set-aside one that matches (after a rollback) |
| `/usr/lib/NetworkManager/dispatcher.d/90-tilo-codec-pack` | A connection comes up, or connectivity becomes `FULL` | Starts the refresh unit (`--no-block`) when the owner opted in and the pack is not installed yet |

The helper validates its single argument (`install`, `remove`, `refresh`,
`guard`, `status`); polkit (`49-tilo-codec-pack.rules`) lets a local, active
administrator session start only the three `install`/`remove`/`refresh`
instances. The unit is sandboxed and may write only its state, cache and
`/var/lib/extensions`. Merging is systemd-sysext's job: the helper asks PID 1 to
enable and reload `systemd-sysext.service`, so the merge happens in the host's
mount namespace.

Status is `/var/lib/tilo/codec-pack/status.json` (root-owned, world-readable,
replaced atomically):

* `state`: `not-installed`, `waiting-for-network`, `installing`, `installed`,
  `removing`, `failed`;
* `desired`: whether the opt-in is recorded;
* `progress`, only while `installing`: `{"step": n, "steps": 7, "label": ...}`,
  the labels being "Checking this device", "Finding packages", "Downloading
  codecs", "Verifying signatures", "Downloading the hardware video driver",
  "Building the extensions" and "Activating";
* `mesa` (`off`, `enabled`, `waiting`, `unsupported`), `message`, `last_error`,
  the base it was built for, the package list, the components still `waiting`
  for a build, and a `generation` that increases after every merge or unmerge.

A failed background refresh leaves a working pack `installed` and reports
`last_error`. The state directory exists on every boot (tmpfiles.d), so Settings
and first-login setup follow the file with inotify rather than polling.

## Offline

Opting in never needs a connection at that moment. When the owner opted in and
NetworkManager reports no internet (`nmcli -t -f CONNECTIVITY general` is
`none`, `limited` or `portal`; no answer counts as online), the helper records
`waiting-for-network` with a plain message and exits successfully: it is not an
error. The choice persists across reboots. A download that fails because the
connection dropped is treated the same way. A pack that is already installed
simply stays `installed`.

The retry is event-driven: NetworkManager runs `90-tilo-codec-pack` on network
events, and when a connection comes up or connectivity reaches `FULL` it starts
the refresh unit, which installs the pack. The boot and daily timer retry as
well. The desktop notices the status change and tells the owner once the codecs
are installed (and, for the Mesa part, that screen sharing and recording pick
it up after signing out and back in).

## Installer handoff

The installer runs from the live medium and installs the image it carries; it
downloads nothing. With the owner's opt-in on its Options step it passes
`--codecs` to `tilo-install`, which writes the root-owned seed
`/etc/tilo/codec-pack-opt-in` into the installed deployment's writable `/etc`.
On the installed system the helper adopts the seed once (on its first refresh
or install): it records the opt-in in `/var/lib/tilo/codec-pack/enabled` and
marks the seed adopted (`installer-seed-adopted`), so removing the pack later
is never undone by the seed. The first refresh after first boot, or
NetworkManager reaching the internet, then installs the pack.

## When the codecs appear

Every media entry point (playback engines, the next `open()` of an existing
engine, real-time encoders and decoders for calls and screen sharing, encoder
selection) reads a stamp of `/usr/lib/extension-release.d/` (names and
modification times; `crates/platform/tilo-media/src/extensions.rs`). When it differs
from what the process last saw, or on a process's first look from the stamp
saved per user in `$XDG_CACHE_HOME/tilo/media-extensions.stamp`, the process
drops every GStreamer plugin it has not loaded from the registry and rescans.
A plain registry update is not enough: GStreamer keys its cache by plugin file
mtime and size, so `libavcodec-freeworld` replacing a library under an
unchanged `libgstlibav.so` would otherwise keep the old feature list, in
running processes and in processes started later.

So new plugins (OpenH264, the freeworld and ugly sets) and plugins not yet
loaded are usable at once, in running apps too, the next time they start a
call, a share or a video. What cannot change inside a running process is a
plugin it has already loaded: a replaced libavcodec under a loaded libav
plugin, and the H.264/HEVC VA-API entry points of the freeworld Mesa driver
under a loaded `va` plugin (libva probes the driver once per process). Those
apply to processes started after the merge. The compositor itself encodes
screen sharing and recordings, so for the Mesa part the desktop tells the
owner exactly that: apps opened from now on have hardware H.264/HEVC; sign out
and back in for screen sharing and recording, and reopen apps that were open.
Without the Mesa part no restart is needed.

## The sealed image

The production image is sealed: the UKI's signed command line pins the composefs
digest of the root filesystem (`composefs=<digest>`, mounted
`overlay ro,verity=require`). systemd-sysext does not change that filesystem. It
mounts a read-only overlayfs on `/usr` whose lowest layer is the sealed `/usr`,
which is the same stacking the kernel allows for any overlay on overlay (two
levels, the kernel's limit). The development image is unsealed because its image
*layers* change the digest (see `os/scripts/unseal-rootfs.py`), not because
merging an extension would. So the pack is expected to merge on the sealed
image, with these consequences:

* `systemd-sysext.service` is not enabled in the production image; the helper
  enables it when the owner opts in.
* The composefs seal and PCR 11 measure the image, not the extension. Extension
  files are verified once, when they are built (RPM signatures), and are then
  plain files on `/var`. This is the trade the owner opts into; the base itself
  stays sealed and a `remove` restores the exact sealed `/usr`.

This has not yet been exercised on a booted sealed image. If merging turns out
not to work there, the fallbacks are, in order of preference: a narrow
read-only overlay mount of only the directories the pack adds to (the same
mechanism, scoped); or a locally derived bootc deployment (base digest plus the
verified RPMs), resealed with the device's own Secure Boot key by the existing
local signing path and activated with a reboot and normal rollback protection.

## Relationship to Tilo system extensions

`/usr/share/tilo/system-extensions/` describes Tilo-signed packs: catalogue
entries signed by Tilo, pinned to an exact base digest, staged as desired state
by `tilo-system-extension-manager`. The codec pack is deliberately not one of
them. Its contents are third-party packages Tilo does not sign or distribute,
fetched only because the owner asked, and resolved on the device against the
publishers' own repositories and keys. It shares only the mechanism, a system
extension over `/usr`, and the principle that the base image stays immutable.
