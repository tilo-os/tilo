#!/usr/bin/env bash
# Run what CI / affected packages runs for this branch, locally, with the
# same package selection, shards, features, filters and environment, so a
# push does not find a failure CI would have.
#
#   tdev bash tools/ci/local-affected.sh [--base REF] [--stage STAGES]
#                                        [--shard N] [--tapps] [--keep-going]
#
#   --base REF     comparison base (default: merge-base with origin/main; run
#                  `git fetch origin` first, and rebase when main moved)
#   --stage LIST   comma list of: check, clippy, test, goldens, certify,
#                  scenarios, scoped, tapps (default: all but tapps)
#   --shard N      only that test/check shard (default: every shard)
#   --tapps        also package the size-budgeted tapps (a release build:
#                  slow) and check them against their budgets
#   --keep-going   continue past a failing step and summarise at the end
#                  (the default stops at the first failure)
#
# Mirrors .forgejo/workflows/check.yml: select (tools/ci/select.sh), check,
# clippy, test (cargo nextest --profile ci through tools/test-env.sh with the
# shared filters), visual goldens, certify, example scenarios, scoped reader
# tests and tapps. Steps CI runs only on a runner (legacy app E2E, the OS
# image, Android, wasm) are not here: tools/ci/check-wasm.sh covers wasm.
set -uo pipefail
cd "$(dirname "$0")/../.."

base=""
stages="check,clippy,test,goldens,certify,scenarios,scoped"
only_shard=""
keep_going=0
want_tapps=0
while [ $# -gt 0 ]; do
  case "$1" in
    --base) base=$2; shift 2 ;;
    --stage) stages=$2; shift 2 ;;
    --shard) only_shard=$2; shift 2 ;;
    --tapps) want_tapps=1; shift ;;
    --keep-going) keep_going=1; shift ;;
    -h | --help) sed -n '2,24p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ "$want_tapps" = 1 ] && stages="$stages,tapps"
[ -n "$base" ] || base=$(git merge-base origin/main HEAD)
base=$(git merge-base "$base" HEAD)

echo "==> select (base $base)"
plan=$(mktemp)
python3 tools/ci/affected_packages.py --base "$base" --shards 2 --allow-security-skip >"$plan" || exit 1
eval "$(python3 tools/ci/nextest_filters.py)"
if [ "$(git rev-parse --abbrev-ref HEAD)" != HEAD ] && ! git merge-base --is-ancestor origin/main HEAD; then
  echo "warning: origin/main is not an ancestor of HEAD; CI tests the merge onto main, so rebase first" >&2
fi

failed=()
step() {
  local label=$1
  shift
  echo "==> $label"
  if ! "$@"; then
    echo "FAILED: $label" >&2
    failed+=("$label")
    [ "$keep_going" = 1 ] || { summary; exit 1; }
  fi
}
summary() {
  if [ ${#failed[@]} -eq 0 ]; then
    echo "==> local affected-packages run: all steps passed"
  else
    printf '==> local affected-packages run FAILED:\n'
    printf '    %s\n' "${failed[@]}"
  fi
}
want() { case ",$stages," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

# One JSON field of the plan, per matrix row, as shell-quoted lines.
rows() { python3 - "$plan" "$1" <<'PY'
import json, shlex, sys
plan = json.load(open(sys.argv[1]))
for row in plan[sys.argv[2]]["include"]:
    print(" ".join(f"{k}={shlex.quote(str(v))}" for k, v in row.items()
                   if isinstance(v, (str, int, bool, float))))
PY
}
field() { # field "k=v k2=v2 ..." key
  python3 - "$1" "$2" <<'PY'
import shlex, sys
for token in shlex.split(sys.argv[1]):
    key, _, value = token.partition("=")
    if key == sys.argv[2]:
        print(value)
PY
}

export TILO_REQUIRE_MEDIA=1 TILO_REQUIRE_AUDIO=1 TILO_REQUIRE_GPU=1
bash tools/ensure-headless-audio.sh >/dev/null 2>&1 || true

if want check; then
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    shard=$(field "$row" shard)
    [ -z "$only_shard" ] || [ "$only_shard" = "$shard" ] || continue
    packages=$(field "$row" packages)
    mkdir -p /tmp/tilo-xdg-runtime && chmod 700 /tmp/tilo-xdg-runtime
    # shellcheck disable=SC2086
    XDG_RUNTIME_DIR=/tmp/tilo-xdg-runtime step "cargo check shard $shard" \
      cargo check --locked --all-targets $packages
    case "$packages " in
      *"-p tilo-ui "*) step "check portable tilo-ui" cargo check --locked -p tilo-ui --no-default-features --lib ;;
    esac
    case "$packages " in
      *"-p tilo-app "*) step "check headless tilo-app" cargo check --locked -p tilo-app --no-default-features --lib ;;
    esac
  done < <(rows check_matrix)
fi

if want clippy; then
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    shard=$(field "$row" shard)
    [ -z "$only_shard" ] || [ "$only_shard" = "$shard" ] || continue
    packages=$(field "$row" packages)
    # shellcheck disable=SC2086
    step "clippy shard $shard" tools/ci/clippy-check.sh $packages
  done < <(rows clippy_matrix)
fi

pull_filter="not ($GOLDEN_FILTER) and not ($QUARANTINE_FILTER) and not ($PUSH_ONLY_FILTER)"
while IFS= read -r row; do
  [ -n "$row" ] || continue
  shard=$(field "$row" shard)
  [ -z "$only_shard" ] || [ "$only_shard" = "$shard" ] || continue
  tests=$(field "$row" test_packages)
  [ -n "$tests" ] || continue
  ntfs=0
  case "$tests" in *"-p tilo-ntfs-helper"*) ntfs=1 ;; esac
  case "$tests" in
    *"-p app-cli"* | *"-p app-host"*)
      want test && step "build tilo-install test fixture (shard $shard)" \
        cargo build --locked -p install --bin tilo-install ;;
  esac
  if want test; then
    # shellcheck disable=SC2086
    TILO_TEST_NTFS_FIXTURES=$ntfs step "nextest shard $shard ($tests)" \
      bash tools/test-env.sh cargo nextest run --locked --profile ci --tests $tests -E "$pull_filter"
  fi
  if want goldens; then
    case " $tests " in
      *" -p shell "* | *" -p tilo-ui "* | *" -p ebook "*)
        # shellcheck disable=SC2086
        step "visual goldens shard $shard" \
          bash tools/test-env.sh cargo nextest run --locked --profile ci --tests $tests --no-tests=pass -E "$GOLDEN_FILTER" ;;
    esac
  fi
  if want certify && [ "$(field "$row" certify)" = True ]; then
    # shellcheck disable=SC2086
    step "tilo-app certify shard $shard" \
      cargo run --locked -p tilo-app-cli --bin tilo-app -- certify --workspace $tests
  fi
  if want scenarios; then
    case "$tests" in
      *"-p example-"* | *"-p tilo-app-cli"*)
        # shellcheck disable=SC2086
        step "example scenarios shard $shard" \
          bash tools/test-env.sh bash tools/ci/example-scenarios.sh $tests ;;
    esac
  fi
  if want scoped; then
    scoped=$(field "$row" scoped_tests)
    if [ -n "$scoped" ]; then
      ntfs=0
      case "$scoped" in *'"tilo-ntfs-helper"'*) ntfs=1 ;; esac
      SCOPED_TESTS=$scoped TILO_TEST_NTFS_FIXTURES=$ntfs \
        step "scoped reader tests shard $shard" python3 tools/ci/run_scoped_tests.py
    fi
  fi
done < <(rows matrix)

if want tapps; then
  affected=$(python3 tools/apps/app_release.py --base "$base" |
    python3 -c 'import json, sys; print(" ".join(json.load(sys.stdin)["apps"]))')
  out=$(mktemp)
  python3 tools/perf/tapp_size_ratchet.py select --apps "$affected" --github-output "$out"
  apps=$(sed -n 's/^tapp_apps=//p' "$out")
  if [ -n "$apps" ]; then
    # As the tapps job: release profile with thin LTO, 16 codegen units.
    CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
      TILO_RELEASE_APPS=$apps step "package tapps ($apps)" dist/apps/package-apps.sh
    step "tapp size budgets" \
      python3 tools/perf/tapp_size_ratchet.py measure dist/app-release --arch "$(uname -m)"
  else
    echo "no tapps selected"
  fi
fi

summary
[ ${#failed[@]} -eq 0 ]
