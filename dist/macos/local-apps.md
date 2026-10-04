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
