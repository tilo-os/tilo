# Qualify the hosted Mac CLI for publication

The agent setup prompt pins an immutable CLI version. Its Mac `tilo` and `tilo-runtime` executables must share access to the same device identity in the account's default login Keychain. An operator with the existing Developer ID Application identity signs the exact two cross-built archives and qualifies the installed programs on each supported Mac architecture. The release worker independently verifies their signatures before it uses KMS or the package token.

## Obtain the exact raw release

Start from the candidate commit's successful `cli-release` and `cli-rehearsal` in `t`. Select the candidate version with `t test --rev "$candidate" cli-rehearsal onboarding --input hosted_version=v0.3.1-assistant.1`. Record the run ID, its complete source commit, its `build-version`, and the four hashes from its `SHA256SUMS`. On the protected tag the trusted policy supplies the same version to all four existing producers. Use this commit's checkout for all commands. The signing tools require Python 3.11 or newer. In the following commands, `work` is an absolute path to an empty operator-owned directory.

```sh
export T_COMMIT="$candidate" T_REF=tag:v+cli-0.3.1-assistant.1
version=v0.3.1-assistant.1
t outputs --run "$candidate_run" --to "$work/build" cli-release
raw="$work/build/target/publish/cli/release"
stage="$work/macos"
mkdir "$stage"
(cd "$raw" && shasum -a 256 -c SHA256SUMS)
```

Obtain the team's authorized Developer ID Application certificate name, public DER SHA-1 and SHA-256 fingerprints, and ten-character team ID from the release administrator's signer inventory. The Linux `t` deployment must independently configure `TILO_HOSTED_APPLE_TEAM_ID` and `TILO_HOSTED_APPLE_LEAF_SHA256` for repository `tilo`, the protected hosted tag and the trusted Linux release node label. These values identify a public certificate. A Mac signing key stays with its operator. The verifier pins Apple's root and Developer ID intermediates.

On the signing Mac, run the native signer on fresh output paths. It compares the program payloads, signs both members with a common product-specific designated requirement, and checks their signatures, certificate, timestamp and entitlements. Keep the raw archives and the verification output for the release review.

```sh
for arch in aarch64 x86_64; do
  archive=tilo-macos-$arch.tar.gz
  raw_sha=$(awk -v name="$archive" '$2 == name {print $1}' "$raw/SHA256SUMS")
  python3 -I dist/apple/hosted-cli-sign.py sign "$raw/$archive" "$raw_sha" "$arch" "$stage/$archive" \
    --team "$team" --identity "$leaf_sha1" --leaf-sha256 "$leaf_sha256" || exit 1
  python3 -I dist/apple/hosted-cli-sign.py verify "$raw/$archive" "$raw_sha" "$arch" "$stage/$archive" \
    --team "$team" --identity "$leaf_sha1" --leaf-sha256 "$leaf_sha256" || exit 1
done
```

## Exercise the signed programs in dedicated accounts

Use a fresh non-root login on a native Intel Mac and on a native Apple silicon Mac. The account needs its normal default `login.keychain-db`, a GUI login for the `launchd` service, and a reachable disposable candidate workspace with a real setup ticket. Remove test-specific identity, secret-store and state-directory environment overrides from the qualification environment. Do not change an item's access policy or grant an interactive Keychain override during the test.

Build an isolated installer mirror of the version directory using the two original Linux archives and the two Mac archives above. Generate a disposable mirror key and checksums in the qualification operator's scratch directory. The production release key is used only by the trusted KMS job later.

```sh
mirror="$work/mirror"
mkdir -p "$mirror/$version"
cp "$raw"/tilo-linux-*.tar.gz "$stage"/tilo-macos-*.tar.gz "$mirror/$version/"
(cd "$mirror/$version" && shasum -a 256 tilo-*.tar.gz > SHA256SUMS)
openssl ecparam -name prime256v1 -genkey -noout -out "$work/mirror.key"
openssl ec -in "$work/mirror.key" -pubout -out "$mirror/release.pub"
openssl dgst -sha256 -sign "$work/mirror.key" -out "$mirror/$version/SHA256SUMS.sig" "$mirror/$version/SHA256SUMS"
```

Copy the public mirror, installer and independently checked public mirror key to each qualification account. Record the default Keychain name returned by `security default-keychain -d user`. Before setup, check that `security find-generic-password -s so.tilo.device -a tilo-device-signing-v1` returns item-not-found. An attributes-only lookup is sufficient. Also confirm that the account and system Tilo identity locations have no `device-private/keypair.bin` fallback. Run the exact candidate installer with the approved ticket in the fresh account and record elapsed time and exit status. The ticket is a secret and must stay outside the receipt, logs and shell history.

```sh
cli="$HOME/.local/share/tilo/cli/$version"
TILO_RELEASES="file://$mirror" TILO_RELEASE_KEY="$mirror/release.pub" TILO_VERSION="$version" \
  TILO_HOSTED_SETUP=1 TILO_INSTALL_DIR="$cli" sh dist/install/tilo.sh login "$ticket"
```

Complete the usual approval in the workspace. Record the installed hashes of `tilo` and `tilo-runtime`. Record `"$cli/tilo" --version` (this candidate reports `tilo 0.3.1-assistant.1`). Run `"$cli/tilo" device public-key` and `"$cli/tilo" status --json`. Require a live `connected` session, the expected workspace and person, a matching daemon `device_public_key`, an absent `answered_ago_s`, and an empty `not_in`. Inspect `launchctl print "gui/$(id -u)/so.tilo.peer"` for the installed runtime path and PID. Restart this disposable service with `launchctl kickstart -k "gui/$(id -u)/so.tilo.peer"`. Repeat the checks after a different PID reconnects with the same device and workspace. Verify the Keychain item now exists via the same attributes-only lookup and the fallback key file remains absent. Record any Keychain permission prompt as a qualification failure.

Write the observed records in `$stage/account-aarch64.json` and `$stage/account-x86_64.json`. The exact schema and field examples are in [the stage verifier's fixture](../tools/tests/test_hosted_cli_stage.py). Record the SHA-256 of the CLI and daemon's respective public key text and the admitted workspace text without trailing newlines. Keep raw credentials, tickets, account paths and key contents out of the release evidence.

## Stage and approve the release

On the operator's Mac, make the qualification receipt from the four original archives, the two signed archives and both account records. Review the source, tag and all hashes. Sign the exact JSON as detached SHA-256 CMS using the approved Developer ID Application identity already in the Keychain. The verifier independently requires that same leaf, team and Apple trust chain on the receipt and every executable.

```sh
qualification_sha=$(python3 -I tools/ci/publish/hosted_cli_stage.py prepare "$raw" "$stage") || exit 1
/usr/bin/security cms -S -T -H SHA256 -u 6 -N "$developer_id_name" \
  -i "$stage/qualification.json" -o "$stage/qualification.cms"
TILO_HOSTED_APPLE_TEAM_ID="$team" TILO_HOSTED_APPLE_LEAF_SHA256="$leaf_sha256" \
  python3 -I tools/ci/publish/hosted_cli_apple.py verify "$raw" "$stage"
```

The last command requires OpenSSL 3 on `PATH`. After release approval, upload exactly `qualification.json`, `qualification.cms`, both `account-ARCH.json` records, and the two `tilo-macos-ARCH.tar.gz` files to `https://code.tilo.so/api/packages/tilo/generic/tilo-cli-macos-stage/$T_COMMIT/` using `tools/upload_immutable_packages.sh` with its normal file/URL manifest. Confirm the public CLI package version is still unoccupied. The release owner must control both the protected tag and its numeric alias so scripts from older commits cannot occupy that immutable package name. The annotated tag `v+cli-0.3.1-assistant.1` must point directly to this candidate commit and contain this single trailer, substituting the digest printed by `prepare`.

```text
Hosted-CLI-Qualification-SHA256: <64 lowercase hexadecimal digits>
```

On the protected tag, `hosted-cli-policy` supplies its version, source and trusted signer pins. `sign-cli` and `publish-cli` each fetch the exact upstream tag and stage independently, verify the source bytes, certificate chain, embedded signatures, common designated requirement and signed qualification, then assemble the same four-archive manifest. Linux checks the certificate chain at release time. The operator’s native Apple verification checks the secure timestamp, and the signed qualification identifies those exact archive bytes. Dispatch `publish-cli` only after review. Immutable writes keep preexisting bytes intact. Stable installs retain their independently released `latest` package. [Dispatch and promotion checks](testing-tilo-sh.md#release-the-cli-used-by-agent-setup)
