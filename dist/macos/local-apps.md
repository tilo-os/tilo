# Unsigned private local app assembly

`collection_local_apps.py` combines one previously compiled app with the reviewed
private runtime payload. It uses the SDK's `tilo-app bundle-macos` command, which
calls the existing macOS assembler without building or signing. The SDK still
checks the manifest's platform, required capabilities, and undeclared capability
usage. Its identity, version, icon, usage descriptions, and entitlements remain
the source of truth.
The executable filename follows the manifest entrypoint, including apps whose
Cargo package and binary have different names without `default-run`.

The app source must be tracked-clean at an explicit commit matching the runtime
profile. Supply the compiled executable's recorded SHA-256, the SDK executable,
the pinned runtime profile, and the staged runtime payload. Compilation receipts
must establish the executable's provenance; a matching hash alone does not prove
which source produced a binary.

Assembly also pins that source's product font catalog and its license notices,
then copies them to `Contents/Resources/fonts`. The exact relative link
`Contents/MacOS/fonts -> ../Resources/fonts` serves the renderer's first catalog
location beside the executable while keeping resource data in Apple's standard
location. This is the only allowed bundle symlink. The source's own font validator checks its
catalog version and hashes; fixture-only fonts are excluded. The compiled-in
manifest must match byte for byte. Missing, changed, executable or symlinked
font resources or a changed lookup link fail verification. This prevents an installed app from depending
on the build checkout for its product fonts, including a checkout under a
privacy-protected directory. Older receipts without a font catalog remain
verifiable as historical artifacts; a fresh assembly always includes it.

```sh
python3 dist/macos/collection_local_apps.py assemble \
  --source-root /path/to/qualified/tilo --revision SOURCE_COMMIT \
  --app calculator --binary /path/to/staged/calculator --binary-sha256 BINARY_SHA256 \
  --sdk /path/to/tilo-app \
  --profile /path/to/reviewed-runtime-profile.json --profile-sha256 PROFILE_SHA256 \
  --payload '/path/to/runtime staging/local-runtime' \
  --out '/path/to/private app staging'
```

`plan` performs read-only validation. `verify` rechecks a completed stage; use
the same arguments. `assemble --resume` accepts only unchanged inputs and an
unchanged output receipt. The output is `app-NAME/NAME.app`, its SDK-generated
entitlements sidecar, a signing inventory, and a hash receipt. It is **ready for
signing preparation**, not yet runtime-qualified or installable.

The generic collection adapter remains strict about its original two-executable
layout. Only this explicit local adapter accepts the 43-image runtime payload
plus the independently validated app executable. It rejects extra code or data,
external app load paths, missing execute permissions, receipt replacements,
helper-name collisions, altered resources, and changed source revisions. It
changes only this private bundle's `LSMinimumSystemVersion` from 13.0 to 27.0.
The general SDK minimum remains 13.0. Names, identifiers, version numbers,
capabilities, and icons are not synthesized by the Python adapter.

Assembly uses the existing offline Cargo metadata path, never a Cargo build or
SDK signing path. Existing outputs are refused, staging uses owned temporary
directories, and the 20 GiB resource floor remains in effect. No app is mounted,
launched, installed, or submitted to Apple.

## Remaining interactive step and subsequent checks

Only the normal Keychain authorization for the existing Developer ID Application
key requires user presence. Do not export keys, unlock the Keychain, change its
ACLs, or bypass the prompt. A first signing call must be bounded so a hidden or
unanswered prompt does not leave a signing process running indefinitely.

After that access is available, the following work is still required but can be
performed by the agent without further blanket approval:

1. Read `signing-inventory.json`; sign the flat libraries/plugins, then scanner,
   real helper and launcher, then the enclosing app with its SDK entitlements.
   Use the exact selected Developer ID Application identity, hardened runtime,
   and secure timestamps. Do not use `--deep` to sign or add entitlement exceptions.
2. Verify every image and the outer bundle against that identity/team and arm64
   architecture. Recheck that app identity, resources and the private minimum
   remain intact.
3. Exercise the locale adapter's forwarding and three domain bindings, an actual
   non-English translation, scanner subprocess discovery, expected codec factories,
   two distinct app locations, and a moved path containing spaces. Verify that
   plugin and catalog reads stay inside the selected bundle. Registry caches are
   keyed by both profile hash and canonical plugin-directory hash.
4. Check ORC/PCRE2 operation under hardened runtime without silently changing JIT
   or library-validation entitlements. Report any app-specific dependency or
   runtime gap before qualifying that app.
5. Only successfully signed and qualified apps may enter the combined DMG or
   optional per-app DMGs. Verify/sign those images and emit hashes and a manifest.
   Do not install unsigned apps or bypass Gatekeeper. Signing is not notarization;
   Apple submission remains a separately authorized operation.

The command intentionally has no `sign`, `install`, or `notarize` action. Its
receipts and signing inventory make the next step concrete while user presence
is pending.

## Tube media profile

Tube's default `media/decode` feature uses the shared media engine's `playbin`,
appsink video path and audio bin. Its normal resolved stream is a progressive
HTTP(S) media URL. A separately pinned `--app-profile` now covers this shape;
it does not disable production features or relax the ordinary SDK adapter.
The same main-executable relocation and inside-out signing inventory used by
Music and Remote apply. Actual playback remains unqualified until signed tests
can run. This profile does not promise adaptive HLS/DASH or additional apps.

The reviewed delta from the 41-image base is 20 plugins: `coreelements`, `app`,
`playback`, `typefindfunctions`, `videoconvertscale`, `videofilter`, `deinterlace`,
`audioconvert`, `audioresample`, `autodetect`, `osxaudio`, `volume`, `audioparsers`,
`videoparsersbad`, `isomp4`, `matroska`, `subparse`, `pango`, `assrender`, `reqwest`.
Their 26 additional libraries and exact 20-package set are enumerated in
`collection_media_runtime.py`. The installed input versions contribute
18,089,696 image bytes, bounded to 24 MiB; any plugin, library, package, hash or
architecture change requires a new reviewed profile. X11 libraries are linked
dependencies of the installed Cairo build; no X server or display is started.

The additional package names are libass, libunibreak, harfbuzz, graphite2,
freetype, libpng, fribidi, cairo, pixman, libxcb, libxdmcp, libxau, libxrender,
libx11, libxext, fontconfig, pango, libthai and libdatrie. Their installed notices
are pinned alongside the libraries. libunibreak's `LICENCE` and libthai's
`share/libthai/thbrk.tri` are explicit hash-pinned data inputs. No package or
dependency installation is performed; this remains a private local profile,
not evidence of public redistribution qualification.

The adapter adds 70 data/notice files (1,377,495 bytes for the reviewed inputs),
including the Good translation catalogs and a source-controlled Fontconfig
configuration. The media launcher selects that configuration and a bundle-local
Thai dictionary. Fontconfig reads `/System/Library/Fonts` and `/Library/Fonts`
in place, with no Homebrew or user configuration includes. It uses the explicit
Pango Fontconfig backend and puts XDG caches under the existing per-profile,
per-location runtime cache. This changes `XDG_CACHE_HOME` only in this media
profile's launched processes. It rejects missing configuration/dictionary paths.
The HTTP plugin uses the native macOS trust path; inherited `SSL_CERT_FILE` and
`SSL_CERT_DIR` overrides are removed. No TLS verification exception is added.

After user presence permits signing, verify local MP4/WebM playback, actual
HTTP range seeking, successful trusted HTTPS and rejected invalid certificates,
text/ASS subtitles, system font discovery, Thai line breaking, locale catalogs,
and a relocated bundle path containing spaces. Observe resource loads to prove
that no build-host/Homebrew configuration, dictionary, module or cache is used.
Static Mach-O closure checks alone do not establish these behaviors. Missing
runtime coverage remains a qualification blocker, never a reason to silently
add plugins, change entitlements, or disable a feature.

Sources: [GStreamer HTTP source](https://gstreamer.freedesktop.org/documentation/reqwest/index.html),
[subtitle overlay](https://gstreamer.freedesktop.org/documentation/playback/subtitleoverlay.html),
[Fontconfig configuration](https://fontconfig.pages.freedesktop.org/fontconfig/fontconfig-user.html),
[Pango backend selection](https://gitlab.gnome.org/GNOME/pango/-/blob/main/pango/pangocairo-fontmap.c),
and [LibThai dictionary selection](https://github.com/tlwg/libthai/blob/master/src/thbrk/brk-common.c).

## Video Player and Video Editor profiles

These are separate app-pinned profiles built on Tube's reviewed media set. A
profile cannot be reused under another app name or silently add package roots.
The launcher and data policy are unchanged. All three media deltas remain
bounded to 24 MiB of additional code; this does not change the Music/Remote limit.

Video Player's file chooser includes AVI alongside MP4, MOV, WebM and Matroska.
Its profile adds `avi` (232,720 bytes), with no additional library or package.
The full delta from the base payload is 47 images (18,322,416 bytes) and the
same 70 data/notice files as Tube. A completed bundle contains 92 code objects.

Video Editor's default features include both preview and export. Its profile
adds `avi`, `x264`, `opus`, `ogg`, `wavparse`, `aiff`, `asf`, `flac`, `flv`,
`mpegpsdemux`, `mpegtsdemux`, `id3demux` and `apetag` beyond Tube. These bring five
libraries: `libgstmpegts`, `libFLAC`, `libogg`, `libgstsdp` and `libgstrtsp`.
Only FLAC and libogg are additional packages; x264 and Opus libraries are already
in the base payload. MP4/MOV export uses the app's existing H.264/AAC selection;
WebM uses VP8/Opus. Adding the installed plugins does not change that selection
policy or introduce a Cisco download or a hardware-encoding claim.

The Editor delta beyond Tube is 18 images (2,780,752 bytes). From the base payload
it is 64 images (20,870,448 bytes) and 168 resource/notice files (1,671,792 bytes),
including the Bad/Ugly catalogs and the new packages' notices. A completed bundle
contains 109 code objects. Package versions, image names, input hashes, notice
hashes and data assets are all validated before copying. An extra package record
or a second version cannot redirect resource collection.

The profiles support static assembly of the default compiled apps, not proof of
every import recognized by their file probes. Some probe extensions intentionally
return unknown metadata in production; this profile does not promise all of
them. After signing access is available, exercise AVI and the usual MP4/WebM/MOV
playback paths, subtitle/font/dictionary behavior, waveform/audio preview, and
real MP4/MOV/WebM exports with audio, cancellation and end-of-stream completion.
Do not call the apps runtime-qualified or installable until those checks pass.

## Music and Remote main-process profiles

These apps directly link GStreamer in their main executable. The ordinary local
adapter continues to reject such loads. For these two apps only, an additional
explicit `--app-profile FILE --app-profile-sha256 SHA256` selects a bounded,
previously audited delta tied to the same source and base-profile hash. Assembly
also requires `--compiler-guard /path/to/existing/rustc-budget.py`. No dependency
discovery silently widens the selected profile during assembly.

Remote adds the `coreelements`, `app`, `videoconvertscale`, and `videoparsersbad`
plugins plus `libgstcodecparsers`. These supply pipeline elements, conversion,
and H.264/HEVC/VP9/AV1 parsers for the existing codec policy. No new package or
encoder policy is introduced; VideoToolbox and Cisco encoder availability are
not implied by this profile.

Music adds `coreelements`, `app`, `playback`, `typefindfunctions`, `audioconvert`,
`audioresample`, `autodetect`, `osxaudio`, `volume`, `audiofx`, `audioparsers`,
`ogg`, `isomp4`, `matroska`, `wavparse`, `aiff`, `asf`, `id3demux`, and `apetag`.
The additional library closure is GStreamer's SDP/RTP/RTSP/RIFF/FFT libraries
and `libogg.0.8.6.dylib`; libogg is the only additional package. Its notice and
the Good/Bad/Ugly translation catalogs are pinned and copied. The locale shim
redirects these additional domains only for the Music profile. This covers the
declared audio-file formats and normal live playback's download-to-file path.
Music's best-effort direct-HTTP CLI diagnostic still reports its missing HTTP
source plugin; this profile does not add curl, libsoup, TLS modules, or network
plugins to make that optional diagnostic pass.

The original SDK executable is preserved as `Contents/MacOS/NAME.real` and its
declared non-system loads are rewritten only to pinned bundle libraries. A
launcher at the unchanged `CFBundleExecutable` configures bundle-local plugins,
scanner, and a profile/location-specific registry before `execv`. The helper uses
the same extended profile in that app. Main and helper launchers share the
existing C implementation; compilers explicitly disable implicit ad-hoc signing.
The SDK plist changes only its private minimum to macOS 27, as before.

The signing inventory now assigns the SDK entitlements to `NAME.real`, while
helper processes retain their empty entitlement policy. Sign this real app and
all other nested code before signing the enclosing bundle/launcher. A valid
static stage is still **unsigned and not runtime-qualified**. After the normal
Keychain gate, qualify actual Music file playback and CoreAudio output, Remote
codec negotiation/decoding, plugin-scanner isolation, catalogs, moved bundle
paths, and hardened runtime before packaging or installation. Additional code or
resources require a new reviewed profile rather than widening these allowlists.

The plugin roles follow the official [playbin](https://gstreamer.freedesktop.org/documentation/playback/playbin.html),
[CoreAudio sink](https://gstreamer.freedesktop.org/documentation/osxaudio/osxaudiosink.html),
[audio parsers](https://gstreamer.freedesktop.org/documentation/audioparsers/index.html),
and [video parsers](https://gstreamer.freedesktop.org/documentation/videoparsersbad/index.html)
documentation. Static dependency closure alone does not prove plugin operation.
