#!/usr/bin/env bash
# Release tilo.sh: main's head by default, as one build of the Lounge, the web entry and the API
# server, running in production. Run it on the build server, from a checkout of tilo/tilo:
#
#   1. t builds and publishes the three images. Its release-tilo-sh check verifies their
#      receipts once the browser flows, onboarding and a fresh stack admitting a page have passed.
#   2. The images' digests go into a checkout of tilo/infra, every pin of each, beside whatever
#      else the checkout changes for this release (a renamed setting: one commit, so Argo never
#      runs a new image with an old environment or the other way round).
#   3. The preflight (tilo_sh_preflight.py) starts production's pods on this machine, with no
#      network: tilo/infra's committed pins on a scratch volume, then the release on that same
#      volume, as Argo's rollout replaces them. Every setting the manifests give must be one the
#      images read, every pod must answer, and the Lounge must stay itself across the upgrade.
#   4. One commit to tilo/infra's main, pushed. Argo rolls it out; this waits until tilo.sh and
#      eu1.tilo.sh report the release's commit.
#
# Each step says how long it took. Nothing here is a tag or a backup: a release is a commit of
# tilo/infra's, and going back is reverting it.
#
# usage: tools/release/tilo-sh.sh [--infra DIR] [--state DIR] [--run RUN | --images LOUNGE WEB API] [--expect-commit SHA] [--no-push]
#   --infra DIR   the tilo/infra checkout to pin in (default: $TILO_INFRA, else ~/work/infra). It
#                 must be at origin/main's head, with only this release's own changes on top.
#   --state DIR   start the preflight's release on a copy of DIR, the live relay's state volume
#                 (read-only), instead of on what the committed pins wrote.
#   --run RUN     follow a release-tilo-sh run already dispatched instead of dispatching one.
#   --expect-commit SHA  require all three published receipts from the tested 40-digit commit.
#                 Use with --run for a release dispatched on the candidate's release tag.
#   --images L W A  pin these three images (by digest) instead of publishing: what an earlier
#                 run of this published, for a release whose step 3 or 4 is run again.
#   --no-push     stop before the commit: the pins and the change stay in the checkout.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../.." && pwd)
infra=${TILO_INFRA:-$HOME/work/infra}
state= push=1 images=() run= expected= source_commit=
while (($#)); do
  case $1 in
    --infra) infra=${2:?}; shift 2 ;;
    --state) state=${2:?}; shift 2 ;;
    --run) run=${2:?}; shift 2 ;;
    --expect-commit) expected=${2:?}; shift 2 ;;
    --images) images=("${2:?}" "${3:?}" "${4:?}"); shift 4 ;;
    --no-push) push=; shift ;;
    -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) echo "tilo-sh.sh: unknown argument $1 (--help)" >&2; exit 2 ;;
  esac
done
say() { printf 'release: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }
started=$SECONDS
lap=$SECONDS
took() { say "$1 in $(((SECONDS - lap) / 60))m $(((SECONDS - lap) % 60))s"; lap=$SECONDS; }

if [[ -n $expected ]]; then
  [[ $expected =~ ^[a-f0-9]{40}$ ]] || die '--expect-commit takes the full 40-digit source commit'
  ((${#images[@]} == 0)) || die '--expect-commit requires the publishing run and its receipts; use --run instead of --images'
fi

apps=(apps/tilo-sh-relay apps/tilo-web apps/tilo-api)
pinned() { # NAME: the one digest tilo/infra's committed tree pins for registry.tilo.so/tilo/NAME
  git -C "$infra" grep -ohE "registry\.tilo\.so/tilo/$1@sha256:[0-9a-f]{64}" HEAD -- "${apps[@]}" | sort -u
}
revision() { # IMAGE: the commit the image says it was built from
  skopeo inspect --no-tags "docker://$1" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["Labels"]["org.opencontainers.image.revision"])'
}

# -- tilo/infra, as production runs it --------------------------------------------------------
git -C "$infra" fetch -q origin main
[[ $(git -C "$infra" rev-parse HEAD) == $(git -C "$infra" rev-parse origin/main) ]] \
  || die "$infra is at $(git -C "$infra" rev-parse --short HEAD), not origin/main: bring it to origin/main first"
# tilo/infra's push checks build every app at the pinned submodules (the Forgejo theme, and the
# old theme inside it), and read them from this checkout: a checkout without them is refused.
git -C "$infra" submodule update -q --init --recursive \
  || die "$infra could not check out its submodules: git -C $infra submodule update --init --recursive"
live_lounge=$(pinned lounge)
[[ -n $live_lounge && $live_lounge != *$'\n'* ]] || die "tilo/infra must pin one Lounge image: $live_lounge"
live=$(revision "$live_lounge")
say "tilo.sh runs $(git -C "$repo" log -1 --format='%h %cs %s' "$live" 2>/dev/null || echo "$live")"

work=${TILO_RELEASE_WORK:-$repo/.claude/scratch/tilo-sh-release}
mkdir -p "$(dirname -- "$work")"

# -- 1. the images -----------------------------------------------------------------------------
if ((${#images[@]} == 0)); then
  if [[ -z $run ]]; then
    say "t publishes main's head (release-tilo-sh): the images, every browser flow, onboarding"
    # The dispatch follows its run to the end; its first line names the run.
    t dispatch release-tilo-sh --ref main --repo tilo --publish > "$work.dispatch" 2>&1 || true
    run=$(grep -oE '\br[0-9a-f]{10}\b' "$work.dispatch" | head -n1)
    [[ -n $run ]] || die "t did not start release-tilo-sh: $(head -c 2000 "$work.dispatch")"
  fi
  say "run $run (t follow $run)"
  t follow "$run" >&2 || die "release-tilo-sh failed: t follow $run, t log --run $run CHECK"
  report=$(t --human log --run "$run" release-tilo-sh) || die "cannot read release-tilo-sh's report from $run"
  source_commit=$(sed -nE 's/^source ([0-9a-f]{40})$/\1/p' <<<"$report")
  [[ -z $source_commit || $source_commit =~ ^[0-9a-f]{40}$ ]] || die "run $run reported multiple source commits"
  [[ -z $expected || $source_commit == "$expected" ]] \
    || die "run $run reports ${source_commit:-no verified source commit}; expected $expected"
  images=()
  while read -r name image; do
    case $name in lounge | web-entry | api) images+=("$image") ;; esac
  done < <(grep -E '^(lounge|web-entry|api) registry\.tilo\.so/' <<< "$report")
  ((${#images[@]} == 3)) || die "release-tilo-sh printed ${#images[@]} of the three images: t log --run $run release-tilo-sh"
  took "published"
fi
lounge=${images[0]} web=${images[1]} api=${images[2]}
lounge_commit=$(revision "$lounge")
commit=${source_commit:-$lounge_commit}
web_commit=$(revision "$web")
# The release's commit and the live one are read here: a fetch that fails is fine as long as this
# checkout already has them.
git -C "$repo" fetch -q origin main || true
for known in "$commit" "$live"; do
  git -C "$repo" cat-file -e "$known^{commit}" 2>/dev/null || die "this checkout lacks commit $known: fetch tilo/tilo's main"
done
# A reused image may name an ancestor of the source the release run tested. The API
# image may carry an empty revision label; its publishing receipt still binds its digest.
check_revision() {
  git -C "$repo" merge-base --is-ancestor "$2" "$commit" \
    || die "$1 is built from $2, outside the release's history at $commit"
}
check_revision "$lounge" "$lounge_commit"
check_revision "$web" "$web_commit"
api_commit=$(revision "$api")
[[ -z $api_commit ]] || check_revision "$api" "$api_commit"
say "the release is $(git -C "$repo" log -1 --format='%h %cs %s' "$commit"), $(git -C "$repo" rev-list --count "$live..$commit") commits after the live build"

# -- 2. the pins -------------------------------------------------------------------------------
for pair in "lounge $lounge" "web-entry $web" "api $api"; do
  read -r name image <<<"$pair"
  digest=${image#*@}
  [[ $image == "registry.tilo.so/tilo/$name@"* && $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a $name image by digest: $image"
  grep -rlE "registry\.tilo\.so/tilo/$name@sha256:" "${apps[@]/#/$infra/}" | while read -r file; do
    python3 -I -c 'from pathlib import Path
import re, sys
path = Path(sys.argv[1])
repository = sys.argv[2].encode().split(b"@", 1)[0]
patched, count = re.subn(re.escape(repository) + b"@sha256:[0-9a-f]{64}", sys.argv[2].encode(), path.read_bytes())
if not count: sys.exit(f"{path} contains no digest pin for {repository.decode()}")
path.write_bytes(patched)' "$file" "$image"
  done
done
git -C "$infra" --no-pager diff --stat >&2

# -- 3. the preflight --------------------------------------------------------------------------
preflight=(uv run -q --no-project --with pyyaml python3 "$here/tilo_sh_preflight.py" --infra "$infra" --work "$work")
[[ -z $state ]] || preflight+=(--state "$state")
"${preflight[@]}" || die "the preflight failed: its pods' logs are in $work/logs; nothing was pushed"
took "preflight passed"

# -- 4. the commit -----------------------------------------------------------------------------
if [[ -z $push ]]; then
  say "not pushed (--no-push): the release is the change in $infra"
  exit 0
fi
short=$(git -C "$repo" rev-parse --short=11 "$commit")
git -C "$infra" add -A -- "${apps[@]}"
git -C "$infra" commit -q -F - <<EOF
deploy(tilo-sh): the Lounge, web entry and API server from tilo $short

$(git -C "$repo" log -1 --format=%s "$commit")

One build of tilo $commit, $(git -C "$repo" rev-list --count "$live..$commit") commits after the
build tilo.sh ran ($(git -C "$repo" rev-parse --short=11 "$live")):
  $lounge
  $web
  $api
EOF
if ! git -C "$infra" push -q origin HEAD:main; then
  git -C "$infra" fetch -q origin main
  if git -C "$infra" merge-base --is-ancestor origin/main HEAD; then
    die "tilo/infra refused the push (its pre-push checks above): the release commit is $(git -C "$infra" rev-parse --short HEAD) in $infra"
  fi
  die "tilo/infra's main moved: run this again with --images $lounge $web $api"
fi
say "pushed tilo/infra $(git -C "$infra" rev-parse --short HEAD)"
took "pushed"

# Argo polls tilo/infra every few minutes, and the relay's pod is recreated (one writer of its
# volume), so tilo.sh is briefly away while the new one starts.
deadline=$((SECONDS + 1200))
for served in "https://eu1.tilo.sh/mode $lounge_commit" "https://tilo.sh/version.json $web_commit"; do
  read -r url want <<<"$served"
  until [[ $(curl -fsS -m 10 "$url" 2>/dev/null | python3 -I -c 'import json,sys; print(json.load(sys.stdin).get("revision",""))' 2>/dev/null) == "$want" ]]; do
    ((SECONDS < deadline)) || die "$url does not report $want after 20 minutes: look at Argo's tilo-sh-vm-host and tilo-web"
    sleep 10
  done
  say "$url serves $(git -C "$repo" rev-parse --short=11 "$want")"
done
# The site sends newcomers to the Lounge's entry by its workspace's id: it must be one the
# Lounge's server lists, or everyone new is turned away instead of meeting the wait list.
entry=$(curl -fsS -m 10 https://tilo.sh/entry.json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["home"]["admission"].rstrip("/").rsplit("/",1)[-1])')
curl -fsS -m 10 -X POST https://eu1.tilo.sh/peer/v1/directory \
  | python3 -I -c 'import json,sys; sys.exit(sys.argv[1] not in {w["entry"].strip("/").rsplit("/",1)[-1] for w in json.load(sys.stdin)["workspaces"]})' "$entry" \
  || die "tilo.sh sends newcomers to workspace $entry, which eu1.tilo.sh does not list: set TILO_WEB_HOME_ENTRY in apps/tilo-web to the Lounge's entry"
say "tilo.sh sends newcomers to the Lounge's entry ($entry)"
took "serving"
say "tilo.sh runs $short, $(((SECONDS - started) / 60)) minutes after the release started"
