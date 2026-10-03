#!/usr/bin/env bash
# app-collection.sh as its own step: built when missing, kept on a rerun from
# the same source, rebuilt after a commit, an uncommitted edit or a failed
# build, and always rebuilt where no git checkout identifies the source.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
arch=$(uname -m)

fail() { echo "test-app-collection: $*" >&2; exit 1; }

# A checkout holding only what the step reads: itself and the app list.
tree=$work/tree
mkdir -p "$tree/dist/rpm" "$tree/tools/apps"
cp "$repo_root/dist/rpm/app-collection.sh" "$tree/dist/rpm/"
cat >"$tree/tools/apps/app_release.py" <<'EOF'
import json
print(json.dumps({"catalog": [{"name": "settings"}, {"name": "notes"}]}))
EOF
echo one >"$tree/README"

# Packaging as dist/apps/package-apps.sh does it: clear OUT, then write the
# collection; counted, and made to fail on request.
cat >"$work/package-apps" <<EOF
#!/usr/bin/env bash
set -euo pipefail
echo "\$TILO_RELEASE_APPS" >>"$work/builds"
mkdir -p "\$TILO_APP_RELEASE_OUT"
find "\$TILO_APP_RELEASE_OUT" -mindepth 1 -delete
[[ -z \${FAKE_FAIL:-} ]] || exit 1
echo collection >"\$TILO_APP_RELEASE_OUT/tilo-apps-$arch.tar.gz"
EOF
chmod +x "$work/package-apps"
export TILO_PACKAGE_APPS=$work/package-apps
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid

out=$work/out
builds() { [[ -f $work/builds ]] && wc -l <"$work/builds" || echo 0; }
step() { "$tree/dist/rpm/app-collection.sh" "$out" >"$work/step.out" 2>&1; }
expect_builds() {
    [[ $(builds) -eq $1 ]] || fail "$2: expected $1 builds, saw $(builds): $(cat "$work/step.out")"
}

# Outside git nothing says what a collection was built from.
step; step
expect_builds 2 "without git"

git -C "$tree" init -q
git -C "$tree" add -A
git -C "$tree" commit -qm one
rm -rf -- "$out" "$work/builds"

step
expect_builds 1 "missing collection"
[[ $(<"$work/builds") == "settings notes" ]] || fail "packaged the wrong apps: $(cat "$work/builds")"
[[ -f $out/tilo-apps-$arch.tar.gz ]] || fail "no collection"
step
expect_builds 1 "rerun from the same commit"
grep -q 'already built' "$work/step.out" || fail "a kept collection did not say so"

echo two >"$tree/README"
step
expect_builds 2 "uncommitted edit"
step
expect_builds 2 "rerun with the same edit"

git -C "$tree" commit -qam two
step
expect_builds 3 "new commit"

# A failed build leaves nothing that a rerun would mistake for fresh.
echo three >"$tree/README"
if FAKE_FAIL=1 step; then fail "a failed build succeeded"; fi
expect_builds 4 "failed build"
step
expect_builds 5 "rerun after a failed build"

# A collection removed by hand is rebuilt even though the record remains.
rm -f -- "$out/tilo-apps-$arch.tar.gz"
step
expect_builds 6 "removed collection"

echo "app collection step contract passed"
