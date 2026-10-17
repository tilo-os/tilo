# Tilo for macOS

For a catalog-driven collection of standalone native SDK apps, see
[the collection script](collection.md). It plans by default and separates build,
sign, package, verify and explicitly authorized notarization stages.

`Tilo.app` is how the standalone per-user runtime reaches a stock Mac. Its
visible app is the Store (`so.tilo.store`: registry search and install),
branded Tilo; beside it is `tilo-runtime`, which every SDK app attaches to.
This is the distributed bundle (`so.tilo.runtime`). The developer shell
bundle from `crates/session/shell/build-macos.sh` (`so.tilo.shell`,
`target/macos/Tilo.app`) is unrelated.

```
Tilo.app/Contents/
  Info.plist                          so.tilo.runtime, version from dist/VERSION
  MacOS/Tilo                          the store binary (CFBundleExecutable), universal
  MacOS/tilo-runtime                  the runtime, universal
  MacOS/tilo-runtime-update           the self-updater, universal
  Library/LaunchAgents/so.tilo.runtime.plist
  Library/LaunchAgents/so.tilo.runtime.update.plist
```

Opening Tilo starts the store, which finds the sibling `tilo-runtime` (the
lookup in `crates/runtime/runtime-portal/src/native.rs`) and re-executes
itself attached to it; the runtime starts its daemon on demand with the
profile `~/Library/Application Support/Tilo/runtime`.

## Build locally

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
dist/macos/build-app.sh                       # -> target/dist-macos/
dist/macos/verify-app.sh target/dist-macos/tilo-macos-universal.dmg
```

`build-app.sh [OUT]` builds both architectures in release, joins them with
`lipo`, signs, and writes `Tilo.app`, `tilo-macos-universal.{zip,dmg}` with
`.sha256` files, and `tilo.rb`, the Homebrew cask rendered from
`homebrew/tilo-cask.rb.in`. The version is `dist/VERSION` unless
`TILO_VERSION` is set (`runtime-v` and `v` prefixes are stripped). The cask
downloads `$TILO_RELEASE_SERVER/$TILO_RELEASE_REPOSITORY/releases/download/runtime-v$VERSION/tilo-macos-universal.dmg`,
by default from `https://github.com/tilo-os/releases`.

`verify-app.sh DMG` mounts the image read-only, checks layout, both
architectures, plists and hardened-runtime signatures, then starts the
bundled daemon on a throwaway profile and HOME and requires its status reply.

## Signing and notarization

Without credentials the bundle is signed ad hoc with the hardened runtime:
it runs on the machine that built it, and elsewhere only after the user
clears quarantine.

Only a Developer ID build's login Keychain items survive a rebuild: an
ad-hoc (or unbundled `cargo build`) runtime's items belong to that exact
build, and reading one another build stored needs the Keychain password.
A runtime without a Developer ID signature therefore never lets the
Keychain ask; it fails at once, naming the item and the `security
delete-generic-password` command that removes it. A debug build keeps its
identity secrets in owner-only files in the profile instead. A locally
built release does so only when started with `TILO_IDENTITY_FILE_SECRETS=1`,
which a Developer ID build ignores. Signing takes the variables `package-host.sh` uses for
Tilo Imager:

| Variable | Effect |
|---|---|
| `TILO_MAC_SIGNING_IDENTITY` | Developer ID Application identity; signs `tilo-runtime`, then the bundle, then the DMG, with secure timestamps |
| `TILO_NOTARY_KEYCHAIN_PROFILE`, or `TILO_APPLE_ID` + `TILO_APPLE_TEAM_ID` + `TILO_APPLE_APP_PASSWORD` | `notarytool submit --wait` for the zip and the DMG, then `stapler staple` and `validate` on both |
| `TILO_MAC_EXPECTED_TEAM_ID`, `TILO_MAC_EXPECTED_CERT_SHA256` | fail unless the signature has this Team ID and leaf certificate |
| `TILO_REQUIRE_SIGNING=1` | fail closed without all of the above; `verify-app.sh` then also requires a stapled ticket and `spctl` acceptance |

Nested code is signed inside out and never with `--deep`. The entitlements in
`Tilo.entitlements` are empty on purpose; `tilo-runtime` alone adds the camera
and microphone (`Runtime.entitlements`), which it captures through AVFoundation
for the apps it confines after the person allows each one. Tilo is not App Sandboxed, because
`tilo-runtime` is the launcher that sandboxes apps. It needs no JIT or
unsigned executable memory, and it loads no third-party dylibs. The file
explains each key it leaves out.

On t, `t dispatch runtime-macos-app` (or `t test runtime-macos-app`) builds
the ad-hoc-signed universal bundle on t's Mac, verifies its DMG with
`verify-app.sh` and keeps the DMG, zip and cask as outputs (`t outputs
runtime-macos-app`). It holds no credentials. Developer ID signing and
notarization are not wired into t yet: they need the signing identity and
notary credentials on a Mac that takes only release jobs, and `build-app.sh`
split so a check without them builds and a separate one, holding them, signs
the built bundle and its DMG. Until then signed builds are made by hand with
`TILO_MAC_SIGNING_IDENTITY` and a notary keychain profile, as above.

## Updates

Apps update through the registry. On Linux the runtime comes from the
distribution repositories. On macOS, `Contents/MacOS/tilo-runtime-update`
(crate `runtime-update`) keeps `Tilo.app` itself current. The
`so.tilo.runtime.update` agent runs it daily and is registered like
`so.tilo.runtime` (see below). It:

1. reads `TiloUpdateFeedURL` and `TiloUpdatePublicKey` from its own
   `Info.plist`, which the bundle signature seals. A bundle without them prints
   `this build updates through its installer` and does nothing, as every
   ad-hoc and local build does.
2. fetches the feed and `FEED.sig`, and verifies the detached Ed25519
   signature over the feed's exact bytes with that key before reading it. The
   feed is `{"version","url","sha256"}`, and the URL must be https.
3. continues only for a numerically newer version. It then downloads the zip,
   checks its SHA-256, and unpacks it beside the installed bundle.
4. requires the new bundle to satisfy the installed bundle's own designated
   requirement, `codesign --verify --deep --strict -R=…`: the same identifier
   and Developer ID team. An ad-hoc bundle's requirement names its own
   hash, so it never accepts a replacement.
5. swaps the bundles with two renames, restores the old one if the second
   fails, and `launchctl kickstart`s `so.tilo.runtime`.

`tilo-runtime-update --check` reports without installing.

Enabling updates for a signed build takes three settings. Configure them on
the Forgejo repository, or export them to `build-app.sh`:

| Setting | `build-app.sh` variable | Content |
|---|---|---|
| `vars.TILO_RUNTIME_UPDATE_FEED` | `TILO_RUNTIME_UPDATE_FEED` | the https URL the feed is served from |
| `vars.TILO_RUNTIME_UPDATE_PUBLIC_KEY` | `TILO_RUNTIME_UPDATE_PUBLIC_KEY` | the key's public half, 64 lowercase hex digits |
| `secrets.TILO_RUNTIME_UPDATE_KEY` | `TILO_RUNTIME_UPDATE_KEY_FILE` (a path) | the signing key, `{"secret_key_hex": …}` |

Create the key with `tilo-runtime-bundle key generate --out KEY` and read the
public half with `tilo-runtime-bundle key pubkey KEY`. With these set,
`build-app.sh` also writes `tilo-macos-update.json` and its `.sig` for the
stapled zip it just built, and the rendered cask declares `auto_updates true`.
The signed workflow run uploads both files as the `tilo-macos-update-feed`
artifact. Publishing is replacing the two files at the feed URL, after the
zip is attached to the `runtime-vX.Y.Z` release. No host serves the feed
yet: choosing one and creating the key are the remaining release steps.

## Homebrew

The cask (`brew install --cask tilo`) is published as a build artifact
(`tilo.rb`), not pushed. `apps/imager/packaging/publish-homebrew-tap.sh` is
bound to `imager-v*` tags and the imager's assets. To publish, attach the
DMG to the `runtime-vX.Y.Z` release of that repository. Then
commit `tilo.rb` as `Casks/tilo.rb` in `tilo-os/homebrew-tilo`, after
`brew audit --strict --cask` on a local tap of it. `depends_on macos:
">= :ventura"` matches `LSMinimumSystemVersion` 13.0. `uninstall` removes
the `so.tilo.runtime` agent. `zap` removes the runtime profile and the
store's data and cache directories. It leaves `~/Library/Application Support/tilo`,
which other Tilo apps share. Identity keys the runtime put in the login
Keychain (service `so.tilo.identity`) are not removed.

## The runtime as a login agent

The daemon does not need launchd: the first app starts it. To keep it running
from login, the agent plist ships inside the bundle.

- **macOS 13+, from code:** `SMAppService.agent(plistName:
  "so.tilo.runtime.plist").register()` from code running in the bundle.
  Nothing calls it yet. launchd resolves
  `BundleProgram` inside the bundle, wherever it is installed, and lists it
  under System Settings, General, Login Items.
- **By hand**, for a copy in `/Applications` (`ProgramArguments[0]`).
  `launchctl bootstrap` fails with `5: Input/output error` on a plist that
  has `BundleProgram`, so the copy drops that key:

  ```sh
  cp /Applications/Tilo.app/Contents/Library/LaunchAgents/so.tilo.runtime.plist ~/Library/LaunchAgents/
  plutil -remove BundleProgram ~/Library/LaunchAgents/so.tilo.runtime.plist
  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/so.tilo.runtime.plist
  launchctl print gui/$(id -u)/so.tilo.runtime     # state = running
  launchctl bootout gui/$(id -u)/so.tilo.runtime && rm ~/Library/LaunchAgents/so.tilo.runtime.plist
  ```

`KeepAlive` restarts the daemon only after a crash. A clean exit is final.
So is a refused second daemon for a profile an app has already started.

The `tilo-store:` URL scheme is not declared. macOS delivers URL opens as
Apple Events, and the store only reads links from its arguments.
