# Signing staged private runtime apps

`collection_staged_sign.py` signs the receipt-backed bundles produced by
`collection_local_apps.py`. It consumes their existing reviewed runtime profiles;
it does not relax the generic collection adapter's two-executable contract.
Use an explicitly selected **Python 3.11+** interpreter. On the current packaging
host, `/opt/homebrew/bin/python3.14` is installed and its imports were checked.

The input is a separately reviewed, SHA-256-pinned selection manifest:

```json
{
  "format": 1,
  "source_root": "/absolute/qualified/source",
  "source": "EXACT_40_CHARACTER_COMMIT",
  "holds": {"path": "/absolute/holds.json", "sha256": "EXACT_SHA256"},
  "apps": [
    {"name": "calculator", "directory": "/absolute/stage/app-calculator", "receipt_sha256": "EXACT_SHA256"}
  ]
}
```

The holds file contains an `apps` object keyed by held catalog names, with reasons.
Every eligible app from the qualified source catalog must appear exactly once,
except held apps. Unsupported platforms and specialized packager exclusions remain
explicit. Partial or duplicate selections, a changed hold, changed receipt,
symlink paths, output overlapping inputs, and dirty/moved source commits fail.
Generate this manifest from the verified staging inventory; do not invent receipt
hashes or remove holds to make a command pass. Updated Notes needs its qualified
source and a newly reviewed manifest, not a changed label on its old binary.

Replace these placeholders with the reviewed paths and public identity:

```sh
/path/to/python3.11-or-newer dist/macos/collection_staged_sign.py plan \
  --manifest /absolute/selection.json --manifest-sha256 EXACT_SHA256 \
  --identity EXACT_DEVELOPER_ID_APPLICATION_SHA1 --out /absolute/signed-output

/path/to/python3.11-or-newer dist/macos/collection_staged_sign.py sign \
  --manifest /absolute/selection.json --manifest-sha256 EXACT_SHA256 \
  --identity EXACT_DEVELOPER_ID_APPLICATION_SHA1 --out /absolute/signed-output --resume

/path/to/python3.11-or-newer dist/macos/collection_staged_sign.py verify \
  --manifest /absolute/selection.json --manifest-sha256 EXACT_SHA256 \
  --identity EXACT_DEVELOPER_ID_APPLICATION_SHA1 --out /absolute/signed-output
```

`plan` hashes receipts and all staged files but does not query Keychain, sign or
launch code. Signing re-runs the existing strict unsigned layout/runtime validator,
regenerates and compares the signing order, then copies one app into an owned
transaction. It checks the copied bytes before key use. Flat libraries and plugins
are signed first, then helper executables, then the outer app. SDK entitlements
apply to the app entrypoint and, when present, the real app executable behind the
launcher. Helpers and libraries receive an explicit empty entitlement policy.
There is no `--deep` signing or preservation of old entitlement metadata.

Only **Developer ID Application** identities are accepted. Every signing call uses
the selected public identity, hardened runtime and Apple's secure timestamp service.
The normal macOS `codesign` key-use prompt may appear; credentials belong only in
that dialog. No unlock, ACL changes, key export/import or notary operation exists
in this entrypoint. Each signing call is bounded to 120 seconds; `--timeout` permits
30–300 seconds for a user-present attempt. Failure stops immediately with no retry.

Post-sign verification checks every Mach-O and the outer bundle with the exact
Apple-anchored leaf requirement, team/authority, runtime flag and timestamp, then
compares actual entitlement dictionaries. Resource bytes and modes must be unchanged;
only the outer CodeResources seal may be added. The thin arm64 comparator checks
every non-signature byte and load command, allowing only the terminal signature,
its command/header padding, and constrained `__LINKEDIT` lengths to change.
The runtime closure and architecture are inspected again. Unsupported Mach-O shapes
fail closed. Fixture tests and actual unsigned inputs establish parser coverage;
they do not claim a completed Developer ID signing run.

Output is `signed-NAME/NAME.app`, its entitlement sidecar and a hash receipt.
Successful apps survive a later app's failure; failed transactions remove only their
owned temporary copies. `--resume` checks hashes and all signatures again before
reuse. The fleet result is written only after every selected app succeeds. Source,
manifest and holds are rechecked before publication. Concurrent fleet writers are
excluded by an output lock. Keep enough space for each copy above the shared 20 GiB
floor. Original staged bundles are never mutated.

Successful signing does **not** claim runtime qualification, installation,
Gatekeeper acceptance or notarization. Media discovery, locale, network, hardened
runtime and app behavior validation remain separate. DMG assembly of these extended
runtime layouts must consume their signed receipts through a reviewed adapter;
do not pass them to the generic two-executable packager or bypass its validation.
