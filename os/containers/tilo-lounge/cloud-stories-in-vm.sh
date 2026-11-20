#!/usr/bin/env bash
# The user stories whose people meet a real server, as t's checks walk them: in
# a micro-VM, as root, this starts the servers the stories need (the official
# Nextcloud image on PostgreSQL, `crates/system/cloud-source-dav/tests/
# servers.sh`, seeded with a person's files, calendars and a second person an
# invitation can name), then walks the story as an unprivileged user on a
# fresh stack of its own (test-user-stories.sh), so this checkout's programs
# start the way they do on a person's machine, not as root.
#
# usage: cloud-stories-in-vm.sh WORK STORY
# Everything else comes from the environment t gives the check (TILO_SH_*).
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
work=${1:?usage: cloud-stories-in-vm.sh WORK STORY}
story=${2:?usage: cloud-stories-in-vm.sh WORK STORY}
[[ $EUID == 0 ]] || {
  echo 'cloud-stories-in-vm.sh starts the servers as root, so it belongs in a VM' >&2
  exit 2
}

# A micro-VM has no systemd: podman manages cgroups itself and logs events to
# a file, as the cloud conformance suite's servers layer does.
mkdir -p /etc/containers/containers.conf.d
printf '[engine]\ncgroup_manager = "cgroupfs"\nevents_logger = "file"\n' \
  > /etc/containers/containers.conf.d/50-micro-vm.conf
command -v podman >/dev/null \
  || dnf install -y --setopt=install_weak_deps=False podman >/dev/null

mkdir -p "$work"
find "$work" -mindepth 1 -delete

# A story whose servers never came up was not walked: its page says it was
# not tested, never that a person met a broken Tilo, and the check fails.
untested() {
  mkdir -p "$work/$story"
  python3 -I -c 'import json, sys, datetime
json.dump({"id": sys.argv[1], "at": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
           "seconds": 0, "completed": False, "withinBudget": False, "brokeAt": None,
           "untested": sys.argv[2], "steps": [], "timings": {}, "videos": []}, open(sys.argv[3], "w"))' \
    "$story" "$1" "$work/$story/story.json"
  python3 -I "$here/stories/page.py" "$work"
  echo "NOT TESTED $story: $1"
  exit 1
}

servers=$repo/crates/system/cloud-source-dav/tests/servers.sh
trap 'bash "$servers" stop' EXIT
# The variables `start` prints are what the suite reads; the seed adds the
# person's own Nextcloud (their address, calendars and files) and the second
# person an invitation can name. Both are exported, so the story's browser and
# driver inherit them. Each is read whole first: a failure inside eval's
# argument would not stop this script.
started=$(bash "$servers" start) || untested "its Nextcloud and calendar servers did not start (the log above says why)"
eval "$started"
seeded=$(bash "$servers" seed) || untested "its Nextcloud could not be given the person's address, calendars and files (the log above says why)"
eval "$seeded"

# The story is walked as an unprivileged user: the stack's services sandbox
# themselves with bwrap, and a program started as root here would prove less
# than the same program on a person's machine.
user=stories
id "$user" >/dev/null 2>&1 || useradd --create-home "$user"
runtime=/tmp/stories-runtime-$user
install -d -m 0700 -o "$user" -g "$user" "$runtime"
chown "$user:$user" "$work"

# The image's Chromium is Playwright's, in root's cache: named for the user,
# whose own home has none, and made reachable.
chrome=${CHROME:-}
if [[ -z $chrome ]]; then
  chrome=$(ls -d "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux*/chrome 2>/dev/null | sort -V | tail -1)
fi
[[ -x $chrome ]] || { echo 'no Chromium in this image (CHROME, or Playwright'"'"'s cache)' >&2; exit 1; }
dir=$chrome
while dir=$(dirname "$dir") && [[ $dir != / ]]; do chmod o+rx "$dir"; done
chmod -R o+rX "$(dirname "$chrome")"

status=0
runuser -u "$user" -- env XDG_RUNTIME_DIR="$runtime" CHROME="$chrome" \
  bash "$here/test-user-stories.sh" "$work" "$story" || status=$?
# What the server said while the story was walked, kept with the recording: a person
# meeting it is the one thing no stack log holds.
if mkdir -p "$work/$story/logs"; then
  podman logs tilo-cloud-conformance-nextcloud > "$work/$story/logs/nextcloud.log" 2>&1 || true
fi
exit "$status"
