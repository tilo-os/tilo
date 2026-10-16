# Private local macOS runtime staging

`collection_local_runtime.py` implements relocation of the reviewed macOS 27,
arm64 helper closure. It produces a reusable **unsigned runtime payload**, not an
app or installer. The general SDK and collection macOS 13 policy is unchanged.
The normal collection validator continues to reject unreviewed nested libraries.

The input is an explicit, SHA-256-pinned local audit profile with 41 images:
`tilo-runtime-mcp`, `gst-plugin-scanner`, five plugins (VPX, AOM, rav1e, dav1d,
libav), and their dependency closure. The smaller profile without libav is not
selected. This preserves the reviewed helper codec discovery surface; it does
not qualify application-specific playback, capture, or export dependencies.

## Commands

Use an existing profile reviewed against the exact source commit and installed
libraries. No profile, binaries, packages, keys, or toolchain are downloaded.
The SHA below is a placeholder for that reviewed file's SHA-256, not a wildcard.

```sh
python3 dist/macos/collection_local_runtime.py plan \
  --profile /path/to/reviewed-profile.json --profile-sha256 SHA256

python3 dist/macos/collection_local_runtime.py stage \
  --profile /path/to/reviewed-profile.json --profile-sha256 SHA256 \
  --compiler-guard /path/to/existing/rustc-budget.py \
  --out '/path/to/private runtime staging'

python3 dist/macos/collection_local_runtime.py verify \
  --profile /path/to/reviewed-profile.json --profile-sha256 SHA256 \
  --out '/path/to/private runtime staging'
```

Prerequisites: macOS 27 arm64, existing Apple command-line tools (`clang`,
`install_name_tool`, `otool`, `lipo`), Python 3.11+, the reviewed installed inputs,
and the existing shared compiler guard. The output filesystem must retain at
least 20 GiB free. `stage --resume` reuses only an unchanged receipt. A failed
stage removes only its own temporary directory; retry uses a fresh transaction.

## Layout and discovery

- `Contents/MacOS/tilo-runtime-mcp.real`: relocated original helper.
- `Contents/MacOS/tilo-runtime-mcp`: small native launcher preserving arguments
  through `execv`; the daemon's later self-reexec inherits discovery variables.
- `Contents/Helpers/gst-plugin-scanner`: relocated scanner.
- `Contents/PlugIns`: exactly the five selected plugin images.
- `Contents/Frameworks`: flattened dependency images and the locale adapter.
- `Contents/Resources/locale`: GLib, GStreamer, and base-plugin message catalogs.
- `Contents/Resources/Notices`: installed license, attribution, and component
  header evidence, including PCRE2's `LICENCE.md` and ORC's complete `COPYING`.

There are **41 original images plus two adapter images**, not 41 total. All
non-system load commands become relative to the loading image; existing rpaths
are removed. The helper and scanner contexts must resolve each original edge to
the same pinned input. Ambiguities, filename collisions, missing inputs,
unexpected architectures, and deployment floors above 27 fail closed.

The launcher sets the versioned and legacy GStreamer plugin, scanner, and
registry variables. It restricts plugin discovery to the bundled directory and
uses an owned, non-group-writable cache directory under the user's
`Library/Caches/so.tilo.local-runtime/<profile SHA>/<canonical plugin path SHA>/`.
Separate app locations and moved bundles therefore have separate registries.
It rejects colon-containing
bundle paths because GStreamer treats colons as path separators. Paths containing
spaces are supported. It resets registry update/fork options and removes an
inherited feature-rank override so the selected profile is consistently scanned.

The installed libintl relocation setter is a no-op. The additional locale dylib
therefore re-exports the real copied libintl and overrides only
`libintl_bindtextdomain`. It redirects the three reviewed domain/original-path
pairs to bundle-relative catalogs. NULL queries, unknown domains, and explicitly
different directories forward unchanged. The scanner receives the same adapter
through its dependency closure. There is no DYLD injection, locale forcing,
library-validation exception, or helper source modification.

## Qualification and signing gate

The script invokes neither `codesign` nor the SDK build path that automatically
ad-hoc signs. Adapter compilation explicitly disables linker ad-hoc signing.
Relocation invalidates existing signatures on copied images. The output receipt
records original hashes, changed load commands, resource hashes, adapter source
hashes, and resulting Mach-O metadata. Static verification is not runtime or
signature verification; the receipt always reports both as pending.

Before integrating this payload into a private app, validate the complete
SDK-generated app and set **that private app's** `LSMinimumSystemVersion` to
`27.0`. Preserve its catalog identity, version, icon, assets and entitlements.
Do not change the general SDK minimum or relax its existing validator merely
because this payload has a receipt. The app's own dependency closure must pass
separately. `collection_local_apps.py` performs this unsigned integration using
the SDK's `bundle-macos` command; see [local app assembly](local-apps.md).

After the user is present for normal Keychain access, sign nested images inside
out with the selected existing Developer ID Application identity, then the app.
Do not unlock/import keys or change ACLs. Before installation, verify every code
object's identity/signature, actually test the shim's forwarding and three domain
bindings, a non-English catalog lookup, scanner subprocess discovery, and the
expected codec factories. Confirm no Homebrew code/catalog reads, including
after moving the app to a path containing spaces. Check ORC/PCRE2 behavior under
hardened runtime without silently adding executable-memory entitlements. These
execution checks are intentionally not performed by this staging command.

No notarization, external redistribution, automatic installation, or launching
occurs here. Installed notices are preserved for this private copy; they are not
a completed external-distribution license/source-offer review. Signing does not
mean notarized. The general collection's separately explicit Apple notarization
stage remains disabled unless independently authorized.

## Sources and tests

The layout follows Apple's [bundle placement guidance](https://developer.apple.com/documentation/bundleresources/placing-content-in-a-bundle).
GStreamer describes dynamic plugin deployment in its [macOS deployment guide](https://gstreamer.freedesktop.org/documentation/deploying/mac-osx.html).
The locale adapter addresses the fixed-directory bindings visible in
[GLib's gettext implementation](https://github.com/GNOME/glib/blob/main/glib/ggettext.c)
and [GStreamer initialization](https://github.com/GStreamer/gstreamer/blob/main/subprojects/gstreamer/gst/gst.c).

Run the focused suite without building or executing bundled code:

```sh
python3 -m unittest discover -s tools/tests -p 'test_macos*py'
```
