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
  PUT:parallel-*.qpkg)
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
  PUT:identical.qpkg)
    : > "$output"
    printf '409'
    ;;
  PUT:conflict.qpkg)
    : > "$output"
    printf '409'
    ;;
  PUT:failed.qpkg)
    printf 'registry unavailable' > "$output"
    printf '503'
    ;;
  PUT:slow-failed.qpkg)
    sleep 0.1
    printf 'slow failure' > "$output"
    printf '500'
    ;;
  PUT:fast-failed.qpkg)
    printf 'fast failure' > "$output"
    printf '502'
    ;;
  PUT:*)
    cp "$upload" "$REMOTE_DIR/$name"
    : > "$output"
    printf '201'
    ;;
  GET:identical.qpkg)
    cp "$upload_fixture" "$output"
    ;;
  GET:conflict.qpkg)
    printf 'different bytes\n' > "$output"
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
    printf '%s\t%s\n' "$work/$1" "https://packages.example.test/exact/$1" >> "$manifest"
    shift
  done
  if tools/upload_immutable_packages.sh "$manifest" >"$work/$name.out" 2>"$work/$name.err"; then
    actual=0
  else
    actual=$?
  fi
  [[ $actual == "$expected" ]]
}

printf 'alpha\n' > "$work/success.qpkg"
: > "$CURL_URL_LOG"
run_case success 0 success.qpkg
cmp "$work/success.qpkg" "$work/remote/success.qpkg"
grep -Fxq 'published success.qpkg' "$work/success.out"
grep -Fxq 'https://packages.example.test/exact/success.qpkg' "$CURL_URL_LOG"

printf 'one\n' > "$work/parallel-one.qpkg"
printf 'two\n' > "$work/parallel-two.qpkg"
printf 'three\n' > "$work/parallel-three.qpkg"
run_case bounded-parallel 0 parallel-one.qpkg parallel-two.qpkg parallel-three.qpkg
[[ $(find "$PARALLEL_DIR" -type f | wc -l) -eq 3 ]]
[[ $(cat "$work/bounded-parallel.out") == $'published parallel-one.qpkg\npublished parallel-two.qpkg\npublished parallel-three.qpkg' ]]

printf 'same\n' > "$work/identical.qpkg"
export upload_fixture="$work/identical.qpkg"
: > "$CURL_URL_LOG"
run_case identical 0 identical.qpkg
grep -Fxq 'already published identically: identical.qpkg' "$work/identical.out"
[[ $(grep -Fxc 'https://packages.example.test/exact/identical.qpkg' "$CURL_URL_LOG") == 2 ]]

printf 'local\n' > "$work/conflict.qpkg"
run_case conflict 1 conflict.qpkg
grep -Fxq 'immutable package version conflicts: https://packages.example.test/exact/conflict.qpkg' \
  "$work/conflict.err"

printf 'first\n' > "$work/first.qpkg"
printf 'failed\n' > "$work/failed.qpkg"
printf 'last\n' > "$work/last.qpkg"
run_case worker-failure 1 first.qpkg failed.qpkg last.qpkg
cmp "$work/first.qpkg" "$work/remote/first.qpkg"
cmp "$work/last.qpkg" "$work/remote/last.qpkg"
grep -Fxq 'immutable package upload returned HTTP 503: https://packages.example.test/exact/failed.qpkg' \
  "$work/worker-failure.err"
[[ $(cat "$work/worker-failure.out") == $'published first.qpkg\npublished last.qpkg' ]]
[[ $(cat "$work/worker-failure.err") == $'registry unavailable\nimmutable package upload returned HTTP 503: https://packages.example.test/exact/failed.qpkg' ]]

printf 'slow\n' > "$work/slow-failed.qpkg"
printf 'fast\n' > "$work/fast-failed.qpkg"
run_case deterministic-errors 1 slow-failed.qpkg fast-failed.qpkg
[[ $(cat "$work/deterministic-errors.err") == $'slow failure\nimmutable package upload returned HTTP 500: https://packages.example.test/exact/slow-failed.qpkg\nfast failure\nimmutable package upload returned HTTP 502: https://packages.example.test/exact/fast-failed.qpkg' ]]

printf 'parallel immutable package upload tests passed\n'
