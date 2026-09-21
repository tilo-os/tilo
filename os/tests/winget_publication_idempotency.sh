#!/usr/bin/env bash
# A tag retry must accept the exact version already merged into WinGet without
# requiring the bot fork to have synchronized its default branch.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d /var/tmp/tilo-winget-idempotency.XXXXXX)"
trap 'find "$work" -depth -delete' EXIT
mkdir -p "$work/bin" "$work/assets/winget"

for name in Tilo.Imager.installer.yaml Tilo.Imager.locale.en-US.yaml Tilo.Imager.yaml; do
  printf 'PackageIdentifier: Tilo.Imager\nPackageVersion: 1.2.3\n# %s\n' "$name" \
    >"$work/assets/winget/$name"
done

python3 - "$work/assets/winget" "$work/accepted.json" <<'PY'
import hashlib
import json
import pathlib
import sys

rows = []
for path in sorted(pathlib.Path(sys.argv[1]).glob("*.yaml")):
    data = path.read_bytes()
    sha = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
    rows.append({"name": path.name, "sha": sha, "type": "file"})
pathlib.Path(sys.argv[2]).write_text(json.dumps(rows), encoding="utf-8")
PY

cat >"$work/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
output=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    *) shift ;;
  esac
done
cp "$TILO_TEST_WINGET_RESPONSE" "$output"
printf '200'
SH
chmod +x "$work/bin/curl"

output="$(PATH="$work/bin:$PATH" \
  TILO_TEST_WINGET_RESPONSE="$work/accepted.json" \
  GITHUB_REF_NAME=imager-v1.2.3 \
  WINGET_GITHUB_TOKEN=test-token \
  WINGET_GITHUB_USER=test-user \
  "$repo/apps/imager/packaging/publish-winget.sh" "$work/assets")"
grep -Fxq 'WinGet already accepted Tilo.Imager 1.2.3' <<<"$output"

python3 - "$work/accepted.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
rows = json.loads(path.read_text(encoding="utf-8"))
rows[0]["sha"] = "0" * 40
path.write_text(json.dumps(rows), encoding="utf-8")
PY
if PATH="$work/bin:$PATH" \
   TILO_TEST_WINGET_RESPONSE="$work/accepted.json" \
   GITHUB_REF_NAME=imager-v1.2.3 \
   WINGET_GITHUB_TOKEN=test-token \
   WINGET_GITHUB_USER=test-user \
   "$repo/apps/imager/packaging/publish-winget.sh" "$work/assets" \
     >"$work/mismatch.out" 2>"$work/mismatch.err"; then
  printf 'WinGet publication accepted different upstream manifests\n' >&2
  exit 1
fi
grep -Fq 'differ from this signed release' "$work/mismatch.err"

printf 'ok    WinGet publication retries accept the exact merged manifests\n'
