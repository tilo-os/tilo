# Tilo File Provider (macOS)

A replicated File Provider extension that shows the standalone runtime's
workspace in Finder under **Locations › Tilo** (`~/Library/CloudStorage/Tilo-Tilo`),
listing every file at once and fetching a file's bytes only when it is first
opened, as the workspace filesystem does on Tilo OS.

```
Tilo.app/Contents/
  MacOS/tilo-files                    adds or removes the File Provider domain
  PlugIns/TiloFiles.appex             the extension (Swift), linking
                                      libtilo_file_provider.a (the Rust bridge)
```

The extension is sandboxed. Its only way out is one SBPL exception
(`TiloFiles.entitlements`) that lets it connect to
`~/Library/Application Support/Tilo/runtime/files.sock`, where `tilo-runtime`
serves the profile's workspace read only (`runtime_fs::workspace_server`).
The bridge (`../src`) holds that connection. It resolves the home directory
from the user database, because a sandboxed process's `HOME` is its container.

This is a skeleton: the extension reads but does not write. Its items are
named by workspace path and versioned by size and modification time. It
reports no changes, so Finder picks up workspace edits only when it
enumerates again.

## Build and try it

```sh
crates/runtime/tilo-file-provider/macos/build.sh                 # dev bundle: target/tilo-file-provider/Tilo.app
crates/runtime/tilo-file-provider/macos/build.sh target/dist-macos/Tilo.app   # or into the distributed app
```

Without an app argument the script builds a minimal bundle with
Tilo.app's identity (`so.tilo.runtime`). Either way it signs ad hoc, unless
`TILO_MAC_SIGNING_IDENTITY` names a Developer ID. To try it:

```sh
app=target/tilo-file-provider/Tilo.app
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app"
pluginkit -a "$app/Contents/PlugIns/TiloFiles.appex"
"$app/Contents/MacOS/tilo-files" add          # prints ~/Library/CloudStorage/Tilo-Tilo
# System Settings › General › Login Items & Extensions › File Providers: enable Tilo
tilo-runtime --daemon &                       # this branch's runtime, default profile
ls ~/Library/CloudStorage/Tilo-Tilo           # enumerates; `cat` a file to hydrate it
"$app/Contents/MacOS/tilo-files" remove
```

## What signing allows, and what is still blocked

Checked on macOS 27 (build 26A428) with an ad hoc signature and no Apple
developer account:

- **Works ad hoc:** the system registers the extension (`pluginkit`). The
  domain is added, and its folder appears under `~/Library/CloudStorage`.
  The sandbox exception lets an ad hoc signed sandboxed process connect to
  the runtime's socket; without it the connect fails with `EPERM`.
- **Blocked on the user, not on signing:** `fileproviderd` refuses to sync
  the new domain (`NSFileProviderErrorDomain` -2011, "domain disabled")
  until the user enables it under System Settings › General › Login Items &
  Extensions › File Providers. `pluginkit -e use` does not enable it, and
  there is no API for it, so no automated test gets past this step. A
  process reading the folder for the first time also triggers a TCC
  consent prompt ("… would like to access files managed by Tilo").
- **Needs a Developer ID to ship:** the SBPL exception is not allowed on the
  Mac App Store, which Tilo does not ship through. An ad hoc signed bundle
  runs only on the Mac that built it. Nothing here needs a provisioning
  profile.

The enumerate-and-hydrate path through the bridge is tested in Rust
(`cargo test -p tilo-file-provider`, `cargo test -p tilo-runtime --lib standalone::files`).
The Swift layer above it was built and registered, but has not enumerated
a domain on a real Mac, because that needs the manual enable step above.
