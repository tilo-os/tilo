#!/usr/bin/env bash
# Move main to what `integration` holds, once it passes.
#
#     tools/integrate.sh
#
# Work in progress is pushed to `integration` without any lint or test (the
# pre-push hook lints only pushes to main), so commits land there at once and
# main is not raced commit by commit. This tests the work once, when it is done:
#
#   1. merges origin/integration onto origin/main, as a merge commit whose first
#      parent is main, so its change is exactly what integration adds;
#   2. runs `t` on that commit: every default check its change touches
#      (`t scope`), and lint-head, which lints the changed packages, the crates
#      that use what they renamed or removed, the packages as they ship and the
#      page's wasm32 build;
#   3. when all pass, pushes the merge to main and to integration in one atomic
#      push (the pre-push hook lints the main side of it, which the warm builds
#      of step 2 make quick); when something fails, prints what failed and the
#      integration commit that first reached it, and pushes nothing.
#
# It is a command a person or an agent runs, not a queue: a run that loses a race
# to another push to main or integration says so, and running it again tests the
# new merge (t's cache makes the checks that already passed free).
#
# It switches this worktree to the merge for the push (the hook lints the files
# on disk) and back afterwards, so the worktree must be clean.
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

# The merge, written without touching the worktree. On a conflict merge-tree
# prints the tree and then the conflicted paths.
if ! merged="$(git merge-tree --write-tree --name-only "$main" "$integration")"; then
  say "integration does not merge cleanly onto main; these files conflict:"
  printf '%s\n' "$merged" | tail -n +2 | sed -n '/./p' | sed 's/^/  /' >&2
  say "merge origin/main into integration, resolve them, push integration and run this again"
  exit 1
fi
tree="$(printf '%s\n' "$merged" | head -1)"
count="$(git rev-list --count "$main..$integration")"
candidate="$(git commit-tree "$tree" -p "$main" -p "$integration" \
  -m "chore: merge integration into main" \
  -m "$(git log --format='- %s' --reverse "$main..$integration" | head -40)")"
say "testing $count integration commit(s) merged onto main ${main:0:12} as ${candidate:0:12}"

# What the merge's change touches: its default checks, and the wide lint.
if ! scope="$(t scope --rev "$candidate" --names)"; then
  say "t cannot tell what the merge touches (above); nothing was pushed"
  exit 1
fi
# shellcheck disable=SC2086 # the check ids are words
if t test --rev "$candidate" lint-head $scope; then
  original="$(git symbolic-ref -q --short HEAD || git rev-parse HEAD)"
  git switch -q --detach "$candidate"
  status=0
  git push --atomic origin "$candidate:refs/heads/main" "$candidate:refs/heads/integration" || status=$?
  git switch -q "$original" 2>/dev/null || git switch -q --detach "$original"
  if [ "$status" -ne 0 ]; then
    say "the push was refused (main or integration moved, or the pre-push lint failed, above); nothing moved. Run this again"
    exit 1
  fi
  say "main is at ${candidate:0:12}: $count integration commit(s) landed"
  exit 0
fi

# Red: name each failure and the integration commit that first reached it. Every
# check t ran already has its result cached, so this asks for them again at once.
# shellcheck disable=SC2086
results="$(mktemp)"
trap 'rm -f "$results"' EXIT
t test --rev "$candidate" --json lint-head $scope >"$results" 2>/dev/null || true
commits="$(git rev-list --reverse "$main..$integration")"
RESULTS="$results" COMMITS="$commits" MAIN="$main" INTEGRATION="$integration" python3 -I - <<'EOF' >&2
import json, os, re, subprocess

def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout.strip()

failed = []
for line in open(os.environ["RESULTS"], encoding="utf-8", errors="replace"):
    if not line.startswith('{"seq"'):
        continue
    finished = json.loads(line).get("body", {}).get("finished")
    if finished and finished.get("outcome") != "pass":
        failed.append(finished)

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
    """The newest integration commit that changed a file the failure names."""
    paths = {path for path in re.findall(r"(?:-->\s*|^)([\w./-]+\.(?:rs|py|toml|sh|tl|ftl)):\d+", text, re.M)
             if os.path.exists(path)}
    found = []
    for path in sorted(paths):
        commit = git("log", "-1", "--format=%H", f"{os.environ['MAIN']}..{os.environ['INTEGRATION']}", "--", path)
        if commit:
            found.append(commit)
    return found

print("integrate: the merge is red; main was not moved")
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
        print("    from: no single commit named; `t log " + check + "` has the whole log")
print("integrate: fix it on integration (push there), then run this again")
EOF
exit 1
