#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
tool=$repo_root/tools/ci/receipt.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git init --bare -q "$work/origin.git"
git init -q -b main "$work/source"
git -C "$work/source" config user.name test
git -C "$work/source" config user.email test@example.invalid
printf 'checked\n' > "$work/source/value"
git -C "$work/source" add value
git -C "$work/source" commit -qm checked
git -C "$work/source" remote add origin "$work/origin.git"
git -C "$work/source" push -q -u origin main
sha=$(git -C "$work/source" rev-parse HEAD)

(cd "$work/source" && "$tool" record "$sha" 42 success success success origin >/dev/null)
(cd "$work/source" && "$tool" verify "$sha" origin)

receipt=$(git -C "$work/source" notes --ref=tilo-check-ci show "$sha")
[[ $receipt == "select=success security=success affected=success run=42 sha=$sha" ]]

if (cd "$work/source" && "$tool" record "$sha" nope success success success origin 2>/dev/null); then
  echo 'receipt tool accepted a non-numeric workflow run id' >&2
  exit 1
fi
if (cd "$work/source" && "$tool" record "$sha" 43 unknown success success origin 2>/dev/null); then
  echo 'receipt tool accepted an unknown job result' >&2
  exit 1
fi

(cd "$work/source" && "$tool" record "$sha" 44 success failure success origin >/dev/null)
if (cd "$work/source" && "$tool" verify "$sha" origin 2>/dev/null); then
  echo 'receipt tool accepted a failed check result' >&2
  exit 1
fi

# The inline receipt reports aggregate outcome, including failures after
# successful selector/security results (for example, an unexpected Rust skip).
(cd "$work/source" && "$tool" record "$sha" 45 success success failure origin >/dev/null)
receipt=$(git -C "$work/source" notes --ref=tilo-check-ci show "$sha")
[[ $receipt == "select=success security=success affected=failure run=45 sha=$sha" ]]
if (cd "$work/source" && "$tool" verify "$sha" origin 2>/dev/null); then
  echo 'receipt tool accepted a failed aggregate outcome' >&2
  exit 1
fi

# Force another writer to advance the remote notes ref immediately before our
# first push. The recorder must fetch that note and retry its own push.
printf 'second\n' > "$work/source/value"
git -C "$work/source" add value
git -C "$work/source" commit -qm second
git -C "$work/source" push -q origin main
git --git-dir="$work/origin.git" symbolic-ref HEAD refs/heads/main
new_sha=$(git -C "$work/source" rev-parse HEAD)
git clone -q "$work/origin.git" "$work/writer-a"
git clone -q "$work/origin.git" "$work/writer-b"
mkdir "$work/wrapper"
cat > "$work/wrapper/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == push && ${2:-} == origin && ${3:-} == refs/notes/tilo-check-ci && ! -e $CI_RECEIPT_TEST_TRIGGER ]]; then
  touch "$CI_RECEIPT_TEST_TRIGGER"
  (cd "$CI_RECEIPT_TEST_OTHER" && PATH="$CI_RECEIPT_TEST_ORIGINAL_PATH" \
    "$CI_RECEIPT_TEST_TOOL" record "$CI_RECEIPT_TEST_OLD_SHA" 46 success success success origin >/dev/null)
fi
exec "$CI_RECEIPT_TEST_REAL_GIT" "$@"
EOF
chmod +x "$work/wrapper/git"
CI_RECEIPT_TEST_TRIGGER="$work/trigger" \
CI_RECEIPT_TEST_OTHER="$work/writer-b" \
CI_RECEIPT_TEST_TOOL="$tool" \
CI_RECEIPT_TEST_OLD_SHA="$sha" \
CI_RECEIPT_TEST_ORIGINAL_PATH="$PATH" \
CI_RECEIPT_TEST_REAL_GIT="$(command -v git)" \
PATH="$work/wrapper:$PATH" \
  bash -c 'cd "$1" && "$2" record "$3" 47 success success success origin >/dev/null' \
  _ "$work/writer-a" "$tool" "$new_sha"
[[ -e $work/trigger ]]
git -C "$work/writer-a" fetch -q origin +refs/notes/tilo-check-ci:refs/notes/tilo-check-ci
[[ $(git -C "$work/writer-a" notes --ref=tilo-check-ci show "$sha") == *'run=46'* ]]
[[ $(git -C "$work/writer-a" notes --ref=tilo-check-ci show "$new_sha") == *'run=47'* ]]

# A forge-side failure that is not a lost race (observed: "remote unpack
# failed") must be retried too, and the receipt must still land.
cat > "$work/wrapper/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == push && ${2:-} == origin && ${3:-} == refs/notes/tilo-check-ci ]]; then
  failures=$(cat "$CI_RECEIPT_TEST_FAILURES")
  if (( failures > 0 )); then
    echo $(( failures - 1 )) > "$CI_RECEIPT_TEST_FAILURES"
    echo 'error: remote unpack failed: unable to create temporary object directory' >&2
    exit 1
  fi
fi
exec "$CI_RECEIPT_TEST_REAL_GIT" "$@"
EOF
echo 3 > "$work/failures"
CI_RECEIPT_TEST_FAILURES="$work/failures" \
CI_RECEIPT_TEST_REAL_GIT="$(command -v git)" \
PATH="$work/wrapper:$PATH" \
  bash -c 'cd "$1" && "$2" record "$3" 48 success success success origin >/dev/null 2>&1' \
  _ "$work/writer-a" "$tool" "$new_sha"
[[ $(cat "$work/failures") == 0 ]]
git -C "$work/writer-b" fetch -q origin +refs/notes/tilo-check-ci:refs/notes/tilo-check-ci
[[ $(git -C "$work/writer-b" notes --ref=tilo-check-ci show "$new_sha") == *'run=48'* ]]

# Coverage: coalescing cancels queued main runs, so a commit may be covered by
# a later commit's green run whose selection base precedes it, but never by a
# run whose base is the commit itself or later, and never by a red run.
git init -q -b main "$work/chain"
git -C "$work/chain" config user.name test
git -C "$work/chain" config user.email test@example.invalid
git -C "$work/chain" remote add origin "$work/origin.git"
git -C "$work/chain" fetch -q origin main
git -C "$work/chain" reset -q --hard origin/main
declare -a c
for index in 1 2 3 4 5 6 7; do
  printf '%s\n' "$index" > "$work/chain/value"
  git -C "$work/chain" commit -qam "commit $index"
  c[index]=$(git -C "$work/chain" rev-parse HEAD)
done
git -C "$work/chain" push -q origin main
record() { (cd "$work/chain" && "$tool" record "$@" origin "${covers:-}" >/dev/null); }
covers='' record "${c[1]}" 50 success success success
covers=${c[1]} record "${c[3]}" 51 success success success
covers=${c[3]} record "${c[5]}" 52 success success failure
covers=${c[5]} record "${c[6]}" 53 success success success

receipt=$(git -C "$work/chain" notes --ref=tilo-check-ci show "${c[3]}")
[[ $receipt == "select=success security=success affected=success run=51 sha=${c[3]}"$'\n\n'"covers=${c[1]}..${c[3]}" ]]
# The range line leaves the exact-commit receipt, and `verify`, unchanged.
(cd "$work/chain" && "$tool" verify "${c[3]}" origin)
covered() { (cd "$work/chain" && "$tool" covers "$1" origin >/dev/null 2>&1); }
covered "${c[1]}"
covered "${c[2]}"
covered "${c[3]}"
# c4's changes were selected only by c5's red run; c6's base is c5.
if covered "${c[4]}"; then
  echo 'coverage accepted a commit whose only covering run was red' >&2
  exit 1
fi
if covered "${c[5]}"; then
  echo 'coverage accepted a red commit through a later run based on it' >&2
  exit 1
fi
covered "${c[6]}"
if (cd "$work/chain" && "$tool" verify "${c[2]}" origin 2>/dev/null); then
  echo 'release verification accepted a commit without its own green run' >&2
  exit 1
fi
# A base that is not a strict ancestor records the exact receipt only.
covers=${c[7]} record "${c[6]}" 55 success success success
[[ $(git -C "$work/chain" notes --ref=tilo-check-ci show "${c[6]}") == "select=success security=success affected=success run=55 sha=${c[6]}" ]]

echo 'CI receipt record/verify/covers contract OK'
