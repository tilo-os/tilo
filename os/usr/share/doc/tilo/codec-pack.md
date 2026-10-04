# Media codecs

The Tilo image ships no patented codecs. Its Mesa is Fedora's, built with
H.264/HEVC disabled: VA-API offers AV1/VP9 decode (and AV1 encode on AMD), but
no H.264 or HEVC. GStreamer is Tilo's only media stack, and without the
`media-codecs` extension it has no H.264/HEVC encoder or decoder beyond what
the stock libav build allows.

`media-codecs` is the first downloaded Tilo extension; the mechanism, its
trust chain and its state are described in
`/usr/share/tilo/system-extensions/README.md`. The owner turns it on with one
button ("Play more video and audio formats") in first-login setup, in the
installer, in Settings, or from a video that cannot be played. The device then
downloads the packages itself, directly from the third parties that publish
them, under their licences. Tilo does not host, mirror or redistribute them:
the image's catalogue entry (`sysext.d/media-codecs.json`) allows artifacts
only from these publishers' hosts.

| Source | Packages |
| --- | --- |
| RPM Fusion free | `gstreamer1-plugins-bad-freeworld`, `gstreamer1-plugins-ugly`, `libavcodec-freeworld` (used by the stock `gstreamer1-plugin-libav`), `mesa-va-drivers-freeworld`, and their dependencies (x264, x265, ...) |
| RPM Fusion nonfree | `intel-media-driver`, and nothing else |
| Cisco, through Fedora's `fedora-cisco-openh264` repository (`codecs.fedoraproject.org`) | `openh264` |
| Fedora | `gstreamer1-plugin-openh264` and dependencies the base lacks |

## How a manifest is made

Tilo CI resolves the extension inside every published image, against the
publishers' repositories (`tools/extensions/`): it checks each package's
upstream OpenPGP signature against pinned fingerprints, excludes every package
the image already has (nothing in the extension may replace a base package),
and pins the result by URL, length and SHA-256 in a manifest it signs with the
extension-manifest key (`/usr/share/tilo/extensions.pub`). CI re-resolves daily, because the publishers' mirrors keep
only the newest build of each package.

The GStreamer plugins follow the base's `gstreamer1` and
`libavcodec-freeworld` follows the base's `libavcodec-free`: the same version
if RPM Fusion has it, otherwise the newest older build of the same
major.minor. The freeworld VA driver brings its own `libgallium-<version>.so`,
which then serves every Mesa client in the process, so it must be built from
exactly the Mesa version the base runs. When RPM Fusion has no such build, the
manifest marks the `tilo-codecs-mesa` part `waiting`; Settings shows "Hardware
H.264/HEVC is waiting for a matching Mesa build", and the stock drivers stay
in use.

## What goes where

Two parts, each a directory system extension under `/var/lib/extensions/`:

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
`ld.so.cache`. OpenH264 needs no move: the image carries Fedora's
`noopenh264` stub (`libopenh264.so.8`, which `libavcodec-free` links), never
Cisco's binary, and Cisco's `openh264` installs its library at the stub's
exact path, so the extension shadows the stub directly. The VA drivers stay in `/usr/lib64/dri-freeworld`, which
Fedora's libva searches before `/usr/lib64/dri`. Each part records what it was
built from in `/usr/share/tilo-extension/<part>.json`; the packages' licence
texts come along in `/usr/share/licenses`.

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

The production image is sealed: the UKI's signed command line pins the
composefs digest of the root filesystem, and `systemd-sysext.service` stays
masked so nothing in `/var` overlays `/usr` by itself. The extension is merged
on purpose by `tilo-extension-activate.service`, after it has rebuilt both
parts from artifacts that match a manifest signed by the extension-manifest key.
systemd-sysext mounts a read-only overlayfs on `/usr` whose lowest layer is the
sealed `/usr`. The composefs seal and PCR 11 measure the image, not the
extension; the extension is as trustworthy as the manifest's signature, checked
again at every activation. Removing it restores the exact sealed `/usr`.
