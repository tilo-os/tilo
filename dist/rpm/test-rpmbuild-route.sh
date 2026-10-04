#!/usr/bin/env bash
# Where rpmbuild.sh runs rpmbuild: on this machine when it has rpmbuild, else in
# the distrobox `.build-container` names, found from the project root above a
# worktree or from the main checkout of a worktree elsewhere; and the advice it
# gives when neither works.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

fail() { echo "test-rpmbuild-route: $*" >&2; exit 1; }

# A PATH with only what the script needs, so the host's own rpmbuild and
# distrobox cannot answer for the fakes.
tools=$work/tools
mkdir -p "$tools"
for tool in bash sh env git dirname head tr cat mkdir chmod rm; do
    ln -s "$(command -v "$tool")" "$tools/$tool"
done
fake_rpmbuild=$work/fake-rpmbuild fake_distrobox=$work/fake-distrobox
mkdir -p "$fake_rpmbuild" "$fake_distrobox"
cat >"$fake_rpmbuild/rpmbuild" <<EOF
#!/usr/bin/env bash
printf '%s\n' "host \$*" >>"$work/log"
EOF
# `distrobox enter NAME -- CMD...`: run CMD as the container would, with an
# rpmbuild there unless FAKE_NO_RPMBUILD is set.
cat >"$fake_distrobox/distrobox" <<EOF
#!/usr/bin/env bash
[[ \$1 == enter && \$3 == -- ]] || exit 2
container=\$2; shift 3
printf '%s\n' "enter \$container" >>"$work/log"
mkdir -p "$work/in-container"
cat >"$work/in-container/rpmbuild" <<'INNER'
#!/usr/bin/env bash
printf '%s\n' "container \$*" >>"$work/log"
INNER
chmod +x "$work/in-container/rpmbuild"
[[ -n \${FAKE_NO_RPMBUILD:-} ]] && rm -f "$work/in-container/rpmbuild"
PATH="$work/in-container:\$PATH" "\$@"
EOF
chmod +x "$fake_rpmbuild/rpmbuild" "$fake_distrobox/distrobox"

export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid
export HOME=$work/home
project=$HOME/project
mkdir -p "$project/dist/rpm"
cp "$repo_root/dist/rpm/rpmbuild.sh" "$project/dist/rpm/"
git -C "$project" init -q
git -C "$project" add -A
git -C "$project" commit -qm init
git -C "$project" worktree add -q "$project/.claude/worktrees/inside"
git -C "$project" worktree add -q "$work/elsewhere"

route() {
    local checkout=$1 path=$2; shift 2
    : >"$work/log"
    PATH=$path "$checkout/dist/rpm/rpmbuild.sh" "$@" >"$work/out" 2>&1
}
log() { cat "$work/log"; }

# rpmbuild on this machine wins, marker or not.
printf 'devbox\n' >"$project/.build-container"
route "$project" "$fake_rpmbuild:$fake_distrobox:$tools" --check || fail "host check: $(cat "$work/out")"
grep -q 'on this machine' "$work/out" || fail "host check: $(cat "$work/out")"
route "$project" "$fake_rpmbuild:$fake_distrobox:$tools" -bb --define '_topdir /a b' x.spec
[[ $(log) == 'host -bb --define _topdir /a b x.spec' ]] || fail "host run: $(log)"

# Without it, the container the marker names, from a worktree below the
# project and from one elsewhere; arguments arrive intact.
for checkout in "$project" "$project/.claude/worktrees/inside" "$work/elsewhere"; do
    route "$checkout" "$fake_distrobox:$tools" --check || fail "$checkout check: $(cat "$work/out")"
    grep -q 'in the build container devbox' "$work/out" || fail "$checkout check: $(cat "$work/out")"
    route "$checkout" "$fake_distrobox:$tools" -bb --define '_topdir /a b' x.spec \
        || fail "$checkout run: $(cat "$work/out")"
    [[ $(log) == $'enter devbox\ncontainer -bb --define _topdir /a b x.spec' ]] \
        || fail "$checkout run: $(log)"
done

# A container without rpmbuild is named before anything is built.
if FAKE_NO_RPMBUILD=1 route "$project" "$fake_distrobox:$tools" --check; then
    fail "accepted a container without rpmbuild"
fi
grep -q 'devbox has no rpmbuild' "$work/out" || fail "no advice: $(cat "$work/out")"

# A marker without distrobox, an empty marker, and no marker at all.
if route "$project" "$tools" --check; then fail "accepted a marker without distrobox"; fi
grep -q 'distrobox, which .* asks for, is not either' "$work/out" || fail "no advice: $(cat "$work/out")"
: >"$project/.build-container"
if route "$project" "$fake_distrobox:$tools" --check; then fail "accepted an empty marker"; fi
grep -q 'is empty; it must name a distrobox' "$work/out" || fail "no advice: $(cat "$work/out")"
rm -f -- "$project/.build-container"
if route "$work/elsewhere" "$fake_distrobox:$tools" --check; then fail "accepted no marker"; fi
grep -Fq "> $project/.build-container" "$work/out" \
    || fail "the advice does not name the main checkout's marker: $(cat "$work/out")"

echo "rpmbuild route contract passed"
