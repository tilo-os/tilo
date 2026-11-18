#!/usr/bin/env bash
# Move main to what `integration` holds, once it passes.
#
#     tools/integrate.sh
#
# Work in progress is pushed to `integration` without any lint or test (the
# pre-push hook lints only pushes to main, and on integration only refuses a
# push that would drop commits), so commits land there at once and main is not
# raced commit by commit. This tests the work once, when it is done:
#
#   1. merges origin/main into origin/integration as a merge commit whose first
#      parent is integration (integration is never rebased or rewritten); when
#      integration already holds main, integration's head is tested as it is;
#   2. runs `t` on that commit: every default check its change touches
#      (`t scope`), and lint-head, which lints what integration adds - the
#      changed packages, the crates that use what they renamed or removed, the
#      packages as they ship and the page's wasm32 build;
#   3. when nothing fails that main does not already fail, pushes the commit to
#      integration and to main in one atomic push. Both are fast-forwards; it
#      never forces either. The pre-push hook lints the main side of it, which
#      the warm builds of step 2 make quick. When something new fails, it
#      prints each failure and the integration commit that first reached it,
#      and pushes nothing.
#
# It is a command a person or an agent runs, not a queue: a run that loses a race
# to another push says so, and running it again tests the new merge (t's cache
# makes the checks that already passed free).
#
# It switches this worktree to the tested commit for the push (the hook lints
# the files on disk) and back afterwards, so the worktree must be clean.
set -euo pipefail

say() { printf 'integrate: %s\n' "$*" >&2; }

root="$(git rev-parse --show-toplevel)"
cd "$root"

if [ -n "$(git status --porcelain)" ]; then
  say "this worktree has uncommitted changes; commit or move them first (the push lints the files on disk)"
  exit 1
fi
command -v t >/dev/null || { say "t is not installed here; it runs the checks"; exit 1; }

git fetch -q origin main integration
main="$(git rev-parse origin/main)"
integration="$(git rev-parse origin/integration)"

if [ -z "$(git rev-list "$main..$integration")" ]; then
  say "integration adds nothing to main; nothing to do"
  exit 0
fi
count="$(git rev-list --count --no-merges "$main..$integration")"

if git merge-base --is-ancestor "$main" "$integration"; then
  candidate="$integration"
  say "testing integration ${integration:0:12} ($count commit(s) beyond main ${main:0:12})"
else
  # The merge, written without touching the worktree. On a conflict merge-tree
  # prints the tree and then the conflicted paths.
  if ! merged="$(git merge-tree --write-tree --name-only "$integration" "$main")"; then
    say "main does not merge cleanly into integration; these files conflict:"
    printf '%s\n' "$merged" | tail -n +2 | sed -n '/./p' | sed 's/^/  /' >&2
    say "merge origin/main into integration yourself (git merge origin/main), resolve them, push integration and start this again"
    exit 1
  fi
  candidate="$(git commit-tree "$(printf '%s\n' "$merged" | head -1)" -p "$integration" -p "$main" \
    -m "chore: merge main into integration")"
  say "testing $count integration commit(s) with main ${main:0:12} merged in, as ${candidate:0:12}"
fi

# What the change touches: its default checks, and the wide lint.
if ! scope="$(t scope --rev "$candidate" --names)"; then
  say "t cannot tell what the change touches (above); nothing was pushed"
  exit 1
fi
results="$(mktemp)"
trap 'rm -f "$results"' EXIT
# shellcheck disable=SC2086 # the check ids are words
t test --rev "$candidate" lint-head $scope || true
# The same results again, as data (all cached by now), to tell what main already fails.
# shellcheck disable=SC2086
t test --rev "$candidate" --json lint-head $scope >"$results" 2>/dev/null || true

# A failure main has no result for is checked on main itself: one that fails there too is main's.
unknown="$(python3 -I - "$results" <<'EOF'
import json, sys
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    if line.startswith('{"seq"'):
        finished = json.loads(line).get("body", {}).get("finished")
        if finished and finished.get("outcome") != "pass" and not isinstance(finished.get("blame"), dict):
            print(finished["check"])
EOF
)"
on_main="$(mktemp)"
trap 'rm -f "$results" "$on_main"' EXIT
if [ -n "$unknown" ]; then
  say "asking main ${main:0:12} for the checks it has no result for: $(echo $unknown | wc -w)"
  # shellcheck disable=SC2086
  t test --rev "$main" --json $unknown >"$on_main" 2>/dev/null || true
fi

commits="$(git rev-list --reverse --no-merges "$main..$integration")"
if RESULTS="$results" ON_MAIN="$on_main" COMMITS="$commits" MAIN="$main" INTEGRATION="$integration" \
    python3 -I - <<'EOF' >&2
import json, os, re, subprocess, sys

def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout.strip()

def finished(path):
    for line in open(path, encoding="utf-8", errors="replace"):
        if line.startswith('{"seq"'):
            body = json.loads(line).get("body", {}).get("finished")
            if body:
                yield body

failing_on_main = {result["check"] for result in finished(os.environ["ON_MAIN"])
                   if result.get("outcome") != "pass"}
failed = [result for result in finished(os.environ["RESULTS"])
          if result.get("outcome") != "pass"
          and not (isinstance(result.get("blame"), dict) and "inherited" in result["blame"])
          and result["check"] not in failing_on_main]
if not failed:
    sys.exit(0)

commits = os.environ["COMMITS"].split()
scopes = {}

def touches(index, check):
    """Whether integration's change up to commits[index] touches the check (t scope grows with it)."""
    if index not in scopes:
        names = subprocess.run(["t", "scope", "--rev", commits[index], "--names"],
                               capture_output=True, text=True).stdout.split()
        scopes[index] = set(names)
    return check in scopes[index]

def first_reaching(check):
    """The first integration commit whose change touches the check, by bisection."""
    if not commits or not touches(len(commits) - 1, check):
        return None
    low, high = 0, len(commits) - 1
    while low < high:
        middle = (low + high) // 2
        if touches(middle, check):
            high = middle
        else:
            low = middle + 1
    return commits[low]

def by_paths(text):
    """The newest integration commit that changed each file the failure names."""
    paths = {path for path in re.findall(r"(?:-->\s*|^\s*)([\w./-]+\.(?:rs|py|toml|sh|tl|ftl|mjs)):\d+", text, re.M)
             if os.path.exists(path)}
    found = []
    for path in sorted(paths):
        commit = git("log", "-1", "--no-merges", "--format=%H",
                     f"{os.environ['MAIN']}..{os.environ['INTEGRATION']}", "--", path)
        if commit:
            found.append(commit)
    return found

print("integrate: these fail with integration and not on main; main was not moved")
for result in failed:
    check = result["check"]
    tests = result.get("failed_tests") or []
    print(f"  {check}  {result.get('outcome')}" + (f"  ({', '.join(map(str, tests[:5]))})" if tests else ""))
    excerpt = (result.get("excerpt") or "").strip()
    for line in excerpt.splitlines()[:12]:
        print(f"      {line}")
    culprits = by_paths(excerpt)
    if not culprits and check != "lint-head":
        reached = first_reaching(check)
        culprits = [reached] if reached else []
    for commit in dict.fromkeys(culprits):
        print(f"    from {git('log', '-1', '--format=%h %an: %s', commit)}")
    if not culprits:
        print(f"    from: no single commit named; `t log {check}` has the whole log")
print("integrate: fix it on integration (push there), then start this again")
sys.exit(1)
EOF
then
  original="$(git symbolic-ref -q --short HEAD || git rev-parse HEAD)"
  git switch -q --detach "$candidate"
  status=0
  git push --atomic origin "$candidate:refs/heads/integration" "$candidate:refs/heads/main" || status=$?
  git switch -q "$original" 2>/dev/null || git switch -q --detach "$original"
  if [ "$status" -ne 0 ]; then
    say "the push was refused (main or integration moved, or the pre-push lint failed, above); nothing moved. Start this again"
    exit 1
  fi
  say "main is at ${candidate:0:12}: $count integration commit(s) landed"
  exit 0
fi
exit 1
