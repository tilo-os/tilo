#!/usr/bin/env bash
# Prove that Linux promotion accepts one pinned key and rejects tampering.
set -euo pipefail

# The signed catalog below names this public fixture origin. CI's checkout
# server can be an internal HTTP endpoint, and its repository is unrelated.
export GITHUB_SERVER_URL=https://code.sandbox.example
export TILO_RELEASE_REPOSITORY=qos/releases

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verify="$repo/apps/imager/packaging/verify-linux-release.sh"
sign="$repo/apps/imager/packaging/sign-linux-release-file.sh"
work="$(mktemp -d /var/tmp/tilo-imager-linux-trust-test.XXXXXX)"
export GNUPGHOME="$work/signing-keyring"
cleanup() {
  # GnuPG may remove its agent sockets while find is walking the temporary
  # keyring. Stop the agent first, then make best-effort cleanup explicitly
  # non-authoritative: trust-test failures come from the assertions above, not
  # an already-vanished socket in this disposable directory.
  gpgconf --homedir "$GNUPGHOME" --kill all >/dev/null 2>&1 || true
  # A deliberately failed production cleanup may leave its private public-key
  # directory behind. Recover only the recorded, tightly scoped fixture path.
  if [[ -f $work/cleanup-keyring ]]; then
    local leftover
    leftover=$(cat "$work/cleanup-keyring")
    if [[ $leftover =~ ^/var/tmp/tilo-imager-release-keyring\.[[:alnum:]]{6}$ && ! -L $leftover ]]; then
      gpgconf --homedir "$leftover" --kill gpg-agent >/dev/null 2>&1 || true
      rm -rf -- "$leftover" || true
    fi
  fi
  find "$work" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT
mkdir -m 0700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-generate-key \
  'Tilo release trust test <release-test@tilo.invalid>' ed25519 sign 1d >/dev/null 2>&1
fingerprint="$(gpg --batch --with-colons --fingerprint \
  | awk -F: '$1 == "pub" { want = 1; next }
               want && $1 == "fpr" { print toupper($10); exit }')"
verify_key="$work/broker-release-key.asc"
gpg --batch --armor --export "$fingerprint" >"$verify_key"
signer="$work/external-signer"
cat >"$signer" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
gpg --batch --yes --armor --detach-sign --output "$2" "$1"
EOF
chmod 0700 "$signer"
external_payload="$work/external-signer-payload"
printf 'OIDC broker signing path\n' >"$external_payload"
TILO_LINUX_SIGNER="$signer" \
TILO_LINUX_VERIFY_KEY="$verify_key" \
TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" \
  "$sign" "$external_payload"
gpg --batch --verify "$external_payload.asc" "$external_payload" >/dev/null 2>&1

assets="$work/assets"
mkdir -p "$assets/linux-x86_64" "$assets/linux-aarch64" \
  "$assets/installer-x86_64" "$assets/installer-aarch64" \
  "$assets/catalog" "$assets/release-key" "$assets/installer-signatures"

for arch in x86_64 aarch64; do
  directory="$assets/linux-$arch"
  for suffix in AppImage tar.gz flatpak; do
    artifact="$directory/tilo-imager-linux-$arch.$suffix"
    printf 'Tilo Imager test artifact: %s %s\n' "$arch" "$suffix" >"$artifact"
    (cd "$directory" && sha256sum "$(basename "$artifact")" \
      >"$(basename "$artifact").sha256")
    gpg --batch --yes --armor --detach-sign "$artifact"
  done
done
for arch in x86_64 aarch64; do
  directory="$assets/installer-$arch"
  artifact="$directory/tilo-installer-linux-$arch.img.zst"
  printf 'Tilo installer test artifact: %s\n' "$arch" >"$artifact"
  (cd "$directory" && sha256sum "$(basename "$artifact")" \
    >"$(basename "$artifact").sha256")
  gpg --batch --yes --armor --detach-sign \
    --output "$assets/installer-signatures/$(basename "$artifact").asc" "$artifact"
done
catalog="$assets/catalog/tilo-installer-catalog-v1.json"
python3 - "$assets" "$catalog" <<'PY'
import hashlib, json, pathlib, sys
root, destination = map(pathlib.Path, sys.argv[1:])
releases = []
for arch in ("x86_64", "aarch64"):
    path = root / f"installer-{arch}/tilo-installer-linux-{arch}.img.zst"
    releases.append({"arch":arch,"hardware_profile":"generic",
        "url":f"https://code.sandbox.example/qos/releases/releases/download/imager-v1.2.3/{path.name}",
        "archive_sha256":hashlib.sha256(path.read_bytes()).hexdigest(),
        "archive_size":path.stat().st_size,"image_sha256":"b"*64,"image_size":1024})
destination.write_text(json.dumps({"schema":1,"tag":"imager-v1.2.3","releases":releases}))
PY
(cd "$assets/catalog" && sha256sum "$(basename "$catalog")" \
  >"$(basename "$catalog").sha256")
TILO_LINUX_SIGNING_KEY="$fingerprint" \
TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" \
  "$sign" "$catalog" \
    "$assets/installer-signatures/$(basename "$catalog").asc"
gpg --batch --armor --export "$fingerprint" \
  >"$assets/release-key/tilo-release-key.asc"
printf '%s\n' "$fingerprint" >"$assets/release-key/tilo-release-key.fingerprint"
(cd "$assets/release-key" && sha256sum tilo-release-key.asc \
  >tilo-release-key.asc.sha256)
(cd "$assets/release-key" && sha256sum tilo-release-key.fingerprint \
  >tilo-release-key.fingerprint.sha256)

# Reproduce an agent socket disappearing between readdir and unlink in the
# production cleanup. The wrapper leaves native GNU find in charge of deletion
# and its exit status; only the concurrent removal is injected deterministically.
cleanup_tools="$work/cleanup-tools"
mkdir "$cleanup_tools"
export TILO_TEST_REAL_FIND="$(command -v find)"
export TILO_TEST_CLEANUP_RACE_LOG="$work/cleanup-race.log"
cat >"$cleanup_tools/find" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == /var/tmp/tilo-imager-release-keyring.* && " $* " == *' -delete '* ]]; then
  printf 'race\n' >>"$TILO_TEST_CLEANUP_RACE_LOG"
  touch "$1/S.gpg-agent.ssh"
  args=()
  for arg in "$@"; do
    if [ "$arg" = -delete ]; then
      args+=(-exec python3 -c 'import os,sys; p=sys.argv[1]; os.unlink(p) if p.endswith("/S.gpg-agent.ssh") else None' '{}' ';')
    fi
    args+=("$arg")
  done
  "$TILO_TEST_REAL_FIND" "${args[@]}"
  exit "${TILO_TEST_CLEANUP_FAILURE:-0}"
fi
exec "$TILO_TEST_REAL_FIND" "$@"
EOF
chmod 0700 "$cleanup_tools/find"
PATH="$cleanup_tools:$PATH" TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
  | grep -Fq 'verified 9 Linux and installer artifacts'
[ "$(cat "$TILO_TEST_CLEANUP_RACE_LOG")" = race ]
if PATH="$cleanup_tools:$PATH" TILO_TEST_CLEANUP_FAILURE=23 \
   TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
   >"$work/cleanup-failure.out" 2>"$work/cleanup-failure.err"; then
  printf 'cleanup failure unexpectedly passed\n' >&2; exit 1
else
  [ "$?" -eq 23 ]
fi
grep -Fq 'verified 9 Linux and installer artifacts' "$work/cleanup-failure.out"
printf 'ok    vanished agent sockets are tolerated and other cleanup errors fail closed\n'

# Exercise cleanup around the actual signed release verification. The wrappers
# model disappearing agent sockets and hard cleanup failures deterministically;
# GnuPG still imports and verifies the real fixture except for one explicit
# import failure used to prove preservation of the original exit status.
mkdir "$work/cleanup-bin"
export TILO_TEST_REAL_GPG="$(command -v gpg)"
export TILO_TEST_REAL_GPGCONF="$(command -v gpgconf)"
export TILO_TEST_REAL_RM="$(command -v rm)"
export TILO_TEST_CLEANUP_STATE="$work/cleanup-keyring"
cat > "$work/cleanup-bin/gpgconf" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ $# == 4 && $1 == --homedir && $3 == --kill && $4 == all ]]
[[ $2 =~ ^/var/tmp/tilo-imager-release-keyring\.[[:alnum:]]{6}$ && $GNUPGHOME == "$2" ]]
printf '%s\n' "$2" > "$TILO_TEST_CLEANUP_STATE"
"$TILO_TEST_REAL_GPGCONF" "$@"
[[ ${TILO_TEST_CLEANUP_MODE:-} != stop-failure ]]
EOF
cat > "$work/cleanup-bin/find" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} != /var/tmp/tilo-imager-release-keyring.* || " $* " != *' -delete '* ]]; then
  exec "$TILO_TEST_REAL_FIND" "$@"
fi
[[ $# == 4 && $1 == "$(cat "$TILO_TEST_CLEANUP_STATE")" \
   && $2 == -ignore_readdir_race && $3 == -depth && $4 == -delete ]]
# Simulate a socket disappearing just before recursive deletion encounters it.
touch "$1/S.gpg-agent.fixture"
"$TILO_TEST_REAL_RM" -f -- "$1/S.gpg-agent.fixture"
if [[ ${TILO_TEST_CLEANUP_MODE:-} == remove-failure ]]; then exit 29; fi
exec "$TILO_TEST_REAL_FIND" "$@"
EOF
cat > "$work/cleanup-bin/gpg" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${TILO_TEST_IMPORT_FAILURE:-0} == 1 && $* == *--import* ]]; then exit 37; fi
exec "$TILO_TEST_REAL_GPG" "$@"
EOF
chmod +x "$work/cleanup-bin/"*
run_cleanup_case() {
  local mode=$1 import_failure=$2 expected_status=$3 status=0 keyring
  "$TILO_TEST_REAL_RM" -f -- "$TILO_TEST_CLEANUP_STATE"
  PATH="$work/cleanup-bin:$PATH" TILO_TEST_CLEANUP_MODE="$mode" \
    TILO_TEST_IMPORT_FAILURE="$import_failure" TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" \
    "$verify" "$assets" >"$work/cleanup.out" 2>"$work/cleanup.err" || status=$?
  [[ $status == "$expected_status" ]] || {
    cat "$work/cleanup.err" >&2
    echo "cleanup case $mode/$import_failure returned $status, expected $expected_status" >&2
    exit 1
  }
  keyring=$(cat "$TILO_TEST_CLEANUP_STATE")
  if [[ $mode == remove-failure ]]; then
    [[ -d $keyring ]]
    "$TILO_TEST_REAL_RM" -rf -- "$keyring"
  else
    [[ ! -e $keyring ]]
  fi
}
run_cleanup_case disappearing-socket 0 0
run_cleanup_case remove-failure 0 29
run_cleanup_case stop-failure 0 1
run_cleanup_case remove-failure 1 37
printf 'ok    isolated keyring cleanup tolerates vanished sockets and preserves trust failures\n'

# Upgrade the actual signed release fixture to a schema-2 board catalog.
cp "$catalog" "$work/v1.json"
mv "$catalog" "$assets/catalog/tilo-installer-catalog-v2.json"
rm "$assets/catalog/tilo-installer-catalog-v1.json.sha256"
find "$assets/installer-signatures" -type f -name '*.asc' -delete
catalog="$assets/catalog/tilo-installer-catalog-v2.json"
board="$assets/installer-aarch64/tilo-installer-linux-aarch64-lenovo-83ed.img.zst"
printf 'Lenovo installer fixture\n' >"$board"
(cd "$(dirname "$board")" && sha256sum "$(basename "$board")" >"$(basename "$board").sha256")
python3 - "$catalog" "$board" <<'PY'
import hashlib, json, pathlib, sys
catalog, board = map(pathlib.Path, sys.argv[1:])
data=json.loads(catalog.read_text()); data["schema"]=2
entry=dict(data["releases"][1]); entry.update(hardware_profile="lenovo-83ed",
    url=f"https://code.sandbox.example/qos/releases/releases/download/imager-v1.2.3/{board.name}",
    archive_sha256=hashlib.sha256(board.read_bytes()).hexdigest(), archive_size=board.stat().st_size,
    provenance={"source_image_id":"sha256:"+"a"*64,"input_qcow2_sha256":"c"*64,
                "hardware_profile_sha256":"d"*64,"target_update_ref":"code.sandbox.example/qos/tilo:lenovo-fixture"})
data["releases"].append(entry); catalog.write_text(json.dumps(data))
PY
(cd "$(dirname "$catalog")" && sha256sum "$(basename "$catalog")" >"$(basename "$catalog").sha256")
for arch in x86_64 aarch64; do
  TILO_LINUX_SIGNING_KEY="$fingerprint" TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" \
    bash "$repo/apps/imager/packaging/sign-installer-release.sh" \
      "$assets/catalog" "$assets" "$arch" "$assets/installer-signatures"
done
TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
  | grep -Fq 'verified 10 Linux and installer artifacts'
printf 'ok    v1 and v2 releases authenticate every generic and board medium\n'
expect_rejected() {
  local label="$1" message="$2"
  if TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
      >"$work/reject.out" 2>"$work/reject.err"; then
    printf '%s unexpectedly passed\n' "$label" >&2; exit 1
  fi
  if [ -n "$message" ]; then grep -Fq "$message" "$work/reject.err"; fi
}
mv "$board" "$work/absent-medium"
expect_rejected 'absent board medium' 'expected exactly one regular catalog medium'
mv "$work/absent-medium" "$board"
board_sig="$assets/installer-signatures/$(basename "$board").asc"
mv "$board_sig" "$work/unsigned-medium.asc"
expect_rejected 'unsigned board medium' 'expected exactly one'
mv "$work/unsigned-medium.asc" "$board_sig"
cp "$work/v1.json" "$assets/catalog/tilo-installer-catalog-v1.json"
expect_rejected 'ambiguous catalog versions' 'expected exactly one installer catalog'
rm "$assets/catalog/tilo-installer-catalog-v1.json"
mv "$catalog" "$assets/catalog/tilo-installer-catalog-v1.json"
expect_rejected 'wrong catalog filename' 'filename does not match its schema'
mv "$assets/catalog/tilo-installer-catalog-v1.json" "$catalog"
catalog_sig="$assets/installer-signatures/$(basename "$catalog").asc"
mv "$catalog_sig" "$work/unsigned-catalog.asc"
expect_rejected 'unsigned catalog' 'expected exactly one'
mv "$work/unsigned-catalog.asc" "$catalog_sig"
cp "$catalog" "$work/valid-catalog"
python3 - "$catalog" <<'PY'
import json, pathlib, sys
path=pathlib.Path(sys.argv[1]); data=json.loads(path.read_text())
data["tag"]="imager-v1.2.4"; path.write_text(json.dumps(data))
PY
(cd "$(dirname "$catalog")" && sha256sum "$(basename "$catalog")" >"$(basename "$catalog").sha256")
expect_rejected 'tampered signed catalog with updated checksum' ''
cp "$work/valid-catalog" "$catalog"
(cd "$(dirname "$catalog")" && sha256sum "$(basename "$catalog")" >"$(basename "$catalog").sha256")
# An updated standalone checksum cannot substitute bytes bound by the signed catalog.
cp "$board" "$work/valid-board"
printf 'board substitution\n' >"$board"
(cd "$(dirname "$board")" && sha256sum "$(basename "$board")" >"$(basename "$board").sha256")
expect_rejected 'catalog media substitution' 'catalog archive'
cp "$work/valid-board" "$board"
(cd "$(dirname "$board")" && sha256sum "$(basename "$board")" >"$(basename "$board").sha256")
printf 'ok    absent, unsigned, ambiguous, mismatched and substituted board releases fail closed\n'

wrong="${fingerprint%?}0"
[ "$wrong" != "$fingerprint" ] || wrong="${fingerprint%?}1"
if TILO_LINUX_EXPECTED_FINGERPRINT="$wrong" "$verify" "$assets" \
   >"$work/wrong.out" 2>"$work/wrong.err"; then
  printf 'wrong protected fingerprint unexpectedly passed\n' >&2
  exit 1
fi
grep -Fq 'published Linux fingerprint does not match protected release policy' \
  "$work/wrong.err"

printf 'tampered\n' >>"$assets/linux-aarch64/tilo-imager-linux-aarch64.AppImage"
if TILO_LINUX_EXPECTED_FINGERPRINT="$fingerprint" "$verify" "$assets" \
   >"$work/tamper.out" 2>"$work/tamper.err"; then
  printf 'tampered artifact unexpectedly passed\n' >&2
  exit 1
fi
grep -Eq 'FAILED|computed checksum did NOT match' "$work/tamper.err"

printf 'ok    Linux release trust bundle fails closed on policy mismatch and tampering\n'

grep -Fq -- '--show-keys --fingerprint' \
  "$repo/apps/imager/packaging/package-host.sh"
grep -Fq -- '--homedir "$verify_home" --import-options import-minimal' \
  "$repo/apps/imager/packaging/package-host.sh"
printf 'ok    OIDC public-key files are imported into isolated verification keyrings\n'
