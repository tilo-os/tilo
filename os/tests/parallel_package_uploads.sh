#!/usr/bin/env bash

set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/bin" "$work/remote" "$work/tmp"

cat > "$work/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
method=GET
upload=
output=
write_out=
header=
url=
while (($#)); do
  case "$1" in
    --request) method=$2; shift 2 ;;
    --upload-file) upload=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    --write-out) write_out=$2; shift 2 ;;
    --header) header=$2; shift 2 ;;
    --retry) shift 2 ;;
    --silent|--show-error|--retry-all-errors|--fail) shift ;;
    *) url=$1; shift ;;
  esac
done
name=${url##*/}
[[ $header == 'Authorization: Bearer masked-test-token' ]]
printf '%s\n' "$url" >> "$CURL_URL_LOG"
case "$method:$name" in
  PUT:parallel-*.tapp)
    : > "$PARALLEL_DIR/$name"
    ready=0
    for _ in {1..100}; do
      if [[ $(find "$PARALLEL_DIR" -type f | wc -l) -ge 3 ]]; then
        ready=1
        break
      fi
      sleep 0.01
    done
    [[ $ready == 1 ]]
    cp "$upload" "$REMOTE_DIR/$name"
    : > "$output"
    printf '201'
    ;;
  PUT:identical.tapp)
    : > "$output"
    printf '409'
    ;;
  PUT:conflict.tapp)
    : > "$output"
    printf '409'
    ;;
  PUT:failed.tapp)
    printf 'registry unavailable' > "$output"
    printf '503'
    ;;
  PUT:slow-failed.tapp)
    sleep 0.1
    printf 'slow failure' > "$output"
    printf '500'
    ;;
  PUT:fast-failed.tapp)
    printf 'fast failure' > "$output"
    printf '502'
    ;;
  PUT:*)
    cp "$upload" "$REMOTE_DIR/$name"
    : > "$output"
    printf '201'
    ;;
  GET:identical.tapp)
    cp "$upload_fixture" "$output"
    printf '200'
    ;;
  GET:conflict.tapp)
    printf 'different bytes\n' > "$output"
    printf '200'
    ;;
  *) exit 88 ;;
esac
EOF
chmod +x "$work/bin/curl"

export PATH="$work/bin:$PATH"
export RUNNER_TEMP="$work/tmp"
export TILO_PACKAGE_TOKEN='masked-test-token'
export TILO_PACKAGE_UPLOAD_WORKERS=3
export CURL_URL_LOG="$work/urls"
export REMOTE_DIR="$work/remote"
export PARALLEL_DIR="$work/parallel"
mkdir -p "$PARALLEL_DIR"

run_case() {
  local name=$1 expected=$2 manifest
  manifest="$work/$name.tsv"
  shift 2
  : > "$manifest"
  while (($#)); do
    printf '%s\t%s\n' "$work/$1" "https://apps.tilo.so.test/artifacts/exact/$1" >> "$manifest"
    shift
  done
  if tools/upload_immutable_packages.sh "$manifest" >"$work/$name.out" 2>"$work/$name.err"; then
    actual=0
  else
    actual=$?
  fi
  [[ $actual == "$expected" ]]
}

printf 'alpha\n' > "$work/success.tapp"
: > "$CURL_URL_LOG"
run_case success 0 success.tapp
cmp "$work/success.tapp" "$work/remote/success.tapp"
grep -Fxq 'published success.tapp' "$work/success.out"
grep -Fxq 'https://apps.tilo.so.test/artifacts/exact/success.tapp' "$CURL_URL_LOG"

printf 'one\n' > "$work/parallel-one.tapp"
printf 'two\n' > "$work/parallel-two.tapp"
printf 'three\n' > "$work/parallel-three.tapp"
run_case bounded-parallel 0 parallel-one.tapp parallel-two.tapp parallel-three.tapp
[[ $(find "$PARALLEL_DIR" -type f | wc -l) -eq 3 ]]
[[ $(cat "$work/bounded-parallel.out") == $'published parallel-one.tapp\npublished parallel-two.tapp\npublished parallel-three.tapp' ]]

printf 'same\n' > "$work/identical.tapp"
export upload_fixture="$work/identical.tapp"
: > "$CURL_URL_LOG"
run_case identical 0 identical.tapp
grep -Fxq 'already published identically: identical.tapp' "$work/identical.out"
[[ $(grep -Fxc 'https://apps.tilo.so.test/artifacts/exact/identical.tapp' "$CURL_URL_LOG") == 2 ]]

printf 'local\n' > "$work/conflict.tapp"
run_case conflict 1 conflict.tapp
grep -Fxq 'immutable package version conflicts: https://apps.tilo.so.test/artifacts/exact/conflict.tapp' \
  "$work/conflict.err"

printf 'first\n' > "$work/first.tapp"
printf 'failed\n' > "$work/failed.tapp"
printf 'last\n' > "$work/last.tapp"
run_case worker-failure 1 first.tapp failed.tapp last.tapp
cmp "$work/first.tapp" "$work/remote/first.tapp"
cmp "$work/last.tapp" "$work/remote/last.tapp"
grep -Fxq 'immutable package upload returned HTTP 503: https://apps.tilo.so.test/artifacts/exact/failed.tapp' \
  "$work/worker-failure.err"
[[ $(cat "$work/worker-failure.out") == $'published first.tapp\npublished last.tapp' ]]
[[ $(cat "$work/worker-failure.err") == $'registry unavailable\nimmutable package upload returned HTTP 503: https://apps.tilo.so.test/artifacts/exact/failed.tapp' ]]

printf 'slow\n' > "$work/slow-failed.tapp"
printf 'fast\n' > "$work/fast-failed.tapp"
run_case deterministic-errors 1 slow-failed.tapp fast-failed.tapp
[[ $(cat "$work/deterministic-errors.err") == $'slow failure\nimmutable package upload returned HTTP 500: https://apps.tilo.so.test/artifacts/exact/slow-failed.tapp\nfast failure\nimmutable package upload returned HTTP 502: https://apps.tilo.so.test/artifacts/exact/fast-failed.tapp' ]]

printf 'parallel immutable package upload tests passed\n'
