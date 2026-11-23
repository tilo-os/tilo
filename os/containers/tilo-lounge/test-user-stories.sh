#!/usr/bin/env bash
# The user stories (stories/stories.json), walked as the people in them would.
# A story says how it is walked (`walk`): in Tilo shells (`shell`: each person a
# headless Tilo session of their own, stories/shell/story.py, or a computer on
# which the `tilo` command is the whole of Tilo, stories/shell/cli_host.py),
# on the web page (`page`: each story on a fresh tilo.sh stack of its own,
# tilo-sh/stack.sh, production's entrypoints and environment over the programs
# t built, as test-e2e.sh brings it up, its people in fresh browser profiles,
# stories/recorder.mjs), or through the workspace's mounted folders (`mount`).
# Every step is recorded, timed and held to its budget. Then a first-time user's
# verdict on each recording (stories/judge.py, a model with no knowledge of the
# code), and one page of every story, green or red (stories/page.py).
#
# usage: test-user-stories.sh WORK [STORY…]
# STORY names the stories to walk (default: every one with a driver). WORK
# keeps each story's recording in WORK/<story>/ (story.json, judge.json, the
# step screenshots, each person's video, a failing step's evidence) with a
# stack's logs in WORK/<story>/logs/, and the page in WORK/index.html, every
# link in it relative to WORK.
#
# A shell story needs the session build TILO_NESTED_SESSION_BUILD names
# (dist/dev/nested-session-build.sh's) and ffmpeg for the videos. A story that
# names `servers` meets a server this script starts beside it as root (podman),
# so its check is a VM, and its driver reads the server's own state as another
# client would. A page story
# needs node, chromium, curl, bwrap, ffmpeg, the stack's environment
# (TILO_SH_PROGRAMS, TILO_SH_LOUNGE, TILO_SH_SITE, TILO_SH_CADDY naming what t
# built) and TILO_CLI_BIN. A story marked `"stack": false` needs none of the
# stack's, only what its driver says it does. The judge needs ANTHROPIC_API_KEY
# and the anthropic Python package: without them a story whose steps pass is
# "passed, not judged", never green, and never red for that alone. Exits
# non-zero when any story walked is red.
set -euo pipefail
usage='usage: test-user-stories.sh WORK [STORY…]'
lounge_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$lounge_dir/../../.." && pwd)
stories=$lounge_dir/stories
tilo_sh=$repo/os/containers/tilo-sh
work=${1:?$usage}
shift

driven=$(python3 -I -c 'import json, sys; print(" ".join(s["id"] for s in json.load(open(sys.argv[1]))["stories"] if s.get("driver")))' "$stories/stories.json")
asked=("$@")
[[ ${#asked[@]} -gt 0 ]] || read -r -a asked <<< "$driven"
for story in "${asked[@]}"; do
  [[ " $driven " == *" $story "* ]] || { echo "no story $story has a driver; those that do: $driven" >&2; exit 2; }
done

mkdir -p "$work"
# How a story is walked: `shell`, `page` or `mount`.
walk_of() { python3 -I -c 'import json, sys; print(next(s for s in json.load(open(sys.argv[1]))["stories"] if s["id"] == sys.argv[2])["walk"])' "$stories/stories.json" "$1"; }
# The servers a story's people meet that this check starts itself (`servers`), if any.
servers_of() { python3 -I -c 'import json, sys; print(next(s for s in json.load(open(sys.argv[1]))["stories"] if s["id"] == sys.argv[2]).get("servers", ""))' "$stories/stories.json" "$1"; }
# Whether a story is walked on a stack of its own: every page story but those marked `"stack": false`.
on_stack() { python3 -I -c 'import json, sys; s = next(s for s in json.load(open(sys.argv[1]))["stories"] if s["id"] == sys.argv[2]); sys.exit(0 if s["walk"] == "page" and s.get("stack", True) else 1)' "$stories/stories.json" "$1"; }
stacked=0
for story in "${asked[@]}"; do on_stack "$story" && stacked=1; done
# The pages draw through WebGPU in software on a node with no GPU, as
# test-e2e.sh's do; a step a person waits for is still held to its own budget.
export TILO_STEP_SECONDS=${TILO_STEP_SECONDS:-20}
if ((stacked)); then
  tilo_bin=${TILO_CLI_BIN:-}
  [[ -x $tilo_bin ]] || { echo 'TILO_CLI_BIN names no tilo command (the stack serves it as its release)' >&2; exit 2; }
  # The stack's release of the command, packed as a release packs it.
  release=$work/release
  mkdir -p "$release"
  bash "$repo/tools/ci/publish/cli-release.sh" pack "$tilo_bin" "$release/tilo-linux-x86_64.tar.gz"
  export TILO_SH_CLI_DIR=$release TILO_SH_TILO_BIN=$tilo_bin
fi
# A story says how its stack's entry takes people, whether it starts the
# stand-in mail server, and whether its stack gets a network namespace of its
# own: `admission` (the stack's TILO_SH_ADMISSION, `open` unless the story asks),
# `mailStandIn`, and `privateNetwork`. In a namespace of its own a stack may bind
# the low ports a mail provider publishes on, which an unprivileged check may
# not on the machine it shares, and it reaches nothing but its own loopback.
# The browsers are started in that namespace too, so they reach the stack it
# holds. People arrive as strangers, the entry open to anyone; no app opened in
# front of them and no VM host.
stacks_for_story() { python3 -I -c '
import json, sys
story = next(s for s in json.load(open(sys.argv[1]))["stories"] if s["id"] == sys.argv[2])
print(story.get("admission", "open"))
print(1 if story.get("mailStandIn") else 0)
print(1 if story.get("privateNetwork") else 0)
' "$stories/stories.json" "$1"; }
export TILO_SH_HOME_APP= TILO_SH_VM_HOST=${TILO_SH_VM_HOST:-0}
export TILO_SH_PORT_BASE=${TILO_SH_PORT_BASE:-25400}

# A story's stack, brought up fresh, walked by its driver, and taken down: the
# whole of it in the namespace the caller is in, so that the browsers the driver
# starts reach the stack and nothing else.
walk_story_stack() {
  : > "$out/walked"
  export TILO_SH_STATE=$out/state
  # A namespace of the story's own starts with its loopback down, where nothing
  # of the stack answers.
  if [[ ${private_network:-0} == 1 ]]; then
    ip link set lo up 2>/dev/null || python3 -I -c '
import fcntl, socket, struct
SIOCGIFFLAGS, SIOCSIFFLAGS, IFF_UP = 0x8913, 0x8914, 0x1
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
flags = struct.unpack("16sH", fcntl.ioctl(sock, SIOCGIFFLAGS, struct.pack("16sH", b"lo", 0)))[1]
fcntl.ioctl(sock, SIOCSIFFLAGS, struct.pack("16sH", b"lo", flags | IFF_UP))
'
  fi
  # The provider's names answer inside this namespace and nowhere else: the
  # address's domain and the name a mail program asks its settings document
  # under, both at this stack's own loopback. They are given before the stack's
  # services start, each in a mount namespace of its own cloned from this one. A
  # test network names its provider the way a real one is named.
  if [[ ${private_network:-0} == 1 ]]; then
    local domain
    domain=$(bash "$tilo_sh/stack.sh" mail-name | sed 's/^[^@]*@//')
    if [[ -z $domain ]]; then
      echo "FAIL: $story: the stack names no stand-in account, so its provider's names are unknown"
      printf '{"id": "%s", "at": "%s", "seconds": 0, "completed": false, "withinBudget": false, "brokeAt": null, "error": "the stack names no stand-in account", "steps": [], "timings": {}, "videos": []}\n' \
        "$story" "$(date -u +%FT%TZ)" > "$out/story.json"
      return 1
    fi
    mkdir -p "$TILO_SH_STATE"
    { cat /etc/hosts
      for name in "$domain" "autoconfig.$domain" "imap.$domain" "smtp.$domain"; do
        printf '127.0.0.1 %s\n::1 %s\n' "$name" "$name"
      done; } > "$TILO_SH_STATE/private-hosts"
    if ! mount --bind "$TILO_SH_STATE/private-hosts" /etc/hosts; then
      echo "FAIL: $story: the namespace would not take its own hosts file"
      return 1
    fi
    echo "the namespace names $domain and autoconfig.$domain at its own loopback"
  fi
  trap 'bash "$tilo_sh/stack.sh" down || true' EXIT
  if bash "$tilo_sh/stack.sh" up --fresh > "$out/stack.log" 2>&1; then
    # shellcheck source=tilo-sh/stack.sh
    . "$tilo_sh/stack.sh"
    # What a client inside this namespace is given of the provider: the names,
    # and the document they answer with, as the stack's own services asked from
    # their sandboxes see it.
    if [[ ${private_network:-0} == 1 ]]; then
      echo "the namespace: getent says $(getent hosts "autoconfig.$domain" 2>&1 | head -1)"
      echo "the namespace: in a sandbox, getent says $(bwrap --dev-bind / / -- getent hosts "autoconfig.$domain" 2>&1 | head -1)"
      echo "the namespace: the document answers $(curl -sS -o /dev/null -w '%{http_code}' --max-time 8 --cacert "$TILO_SH_STATE/trust-bundle.pem" "https://autoconfig.$domain/mail/config-v1.1.xml" 2>&1)"
    fi
    status=0
    STORY_STACK=$tilo_sh/stack.sh node "$stories/$driver" "$site_origin" "$out" "$TILO_SH_STATE"/logs/{server,api,web}.log 2>&1 | tee "$out/driver.log" || status=${PIPESTATUS[0]}
  else
    echo "FAIL: $story: the stack did not come up"
    tail -40 "$out/stack.log"
    printf '{"id": "%s", "at": "%s", "seconds": 0, "completed": false, "withinBudget": false, "brokeAt": null, "error": "the stack did not come up", "steps": [], "timings": {}, "videos": []}\n' \
      "$story" "$(date -u +%FT%TZ)" > "$out/story.json"
    status=1
  fi
  bash "$tilo_sh/stack.sh" down > /dev/null 2>&1 || true
  trap - EXIT
  return "$status"
}

red=()
# The servers a story's people meet, started beside it and stopped after it: a
# story marked `servers` meets a real Nextcloud (servers.sh's, seeded with the
# person's files, calendars and a second person an invitation can name). podman
# starts the containers as root, which is why these stories' check is a VM;
# nothing of them is left behind.
servers_sh=$repo/crates/integrations/cloud-source-dav/tests/servers.sh
serving=
stop_servers() {
  [[ -z $serving ]] || bash "$servers_sh" stop > /dev/null 2>&1 || true
  serving=
}
trap stop_servers EXIT
# What a story whose servers never came up is: not tested, never a person
# meeting a broken Tilo, and the check fails.
untested() {
  local story=$1 why=$2 out=$3
  python3 -I -c 'import json, sys, datetime
json.dump({"id": sys.argv[1], "at": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
           "seconds": 0, "completed": False, "withinBudget": False, "brokeAt": None,
           "untested": sys.argv[2], "steps": [], "timings": {}, "videos": []}, open(sys.argv[3], "w"))' \
    "$story" "$why" "$out/story.json"
  echo "NOT TESTED $story: $why"
}
start_servers() {
  local story=$1 out=$2 servers=$3 started seeded
  [[ $servers == nextcloud ]] || return 0
  if [[ $EUID != 0 ]]; then
    untested "$story" "its Nextcloud starts containers as root, which a sandbox job refuses" "$out"
    return 1
  fi
  # A micro-VM has no systemd: podman manages cgroups itself and logs events to
  # a file, as the cloud conformance suite's servers layer does.
  mkdir -p /etc/containers/containers.conf.d
  printf '[engine]\ncgroup_manager = "cgroupfs"\nevents_logger = "file"\n' \
    > /etc/containers/containers.conf.d/50-micro-vm.conf
  if ! command -v podman > /dev/null; then
    untested "$story" "podman, which starts its Nextcloud, is not installed (the check's setup installs it)" "$out"
    return 1
  fi
  started=$(bash "$servers_sh" start) || {
    untested "$story" "its Nextcloud and its database did not start (the log above says why)" "$out"
    return 1
  }
  eval "$started"
  serving=1
  seeded=$(bash "$servers_sh" seed) || {
    untested "$story" "its Nextcloud could not be given the person's files and calendars (the log above says why)" "$out"
    return 1
  }
  eval "$seeded"
}

for story in "${asked[@]}"; do
  out=$work/$story
  rm -rf "$out"
  mkdir -p "$out"
  driver=$(python3 -I -c 'import json, sys; print(next(s["driver"] for s in json.load(open(sys.argv[1]))["stories"] if s["id"] == sys.argv[2]))' "$stories/stories.json" "$story")
  { read -r TILO_SH_ADMISSION; read -r TILO_SH_MAIL_STAND_IN; read -r private_network; } < <(stacks_for_story "$story")
  export TILO_SH_ADMISSION TILO_SH_MAIL_STAND_IN private_network
  status=0
  unset TILO_MAIL_STAND_IN
  if ! start_servers "$story" "$out" "$(servers_of "$story")"; then
    red+=("$story")
    continue
  fi
  if [[ $(walk_of "$story") == shell ]]; then
    # Each person a headless Tilo session of their own, on a scratch home of its own; the
    # driver keeps what a person reads the story by and removes the sessions' state. A story
    # that rehearses a mail provider is given the tree's stand-in mail server, which the
    # nested-session build staged beside the session. Spool the driver's log
    # locally and move a copy to the shared output mount after the walk.
    if [[ $TILO_SH_MAIL_STAND_IN == 1 ]]; then
      export TILO_MAIL_STAND_IN=${TILO_NESTED_SESSION_BUILD:?}/bin/mail-stand-in
    fi
    driver_log=$(mktemp)
    PYTHONDONTWRITEBYTECODE=1 python3 "$stories/$driver" "$out" 2>&1 | tee "$driver_log" || status=${PIPESTATUS[0]}
    cp "$driver_log" "$out/driver.log" || echo "could not keep $story's driver log" >&2
    rm -f "$driver_log"
  elif ! on_stack "$story"; then
    # A story with no stack walks what it drives itself (its driver says what).
    node "$stories/$driver" "" "$out" 2>&1 | tee "$out/driver.log" || status=${PIPESTATUS[0]}
  else
    # A stack of the story's own, fresh, its state beside the recording, and,
    # where the story asks, in a network namespace of its own.
    status=0
    if [[ $private_network == 1 ]]; then
      # The stack, its browsers and everything they reach share one namespace,
      # with mounts of its own: a host of the check's own would not answer
      # inside it, the ports a provider publishes on are bound there as root
      # may, and the names a provider is known by are its own to give.
      export story driver out tilo_sh stories
      export -f walk_story_stack
      unshare --user --map-root-user --mount --net -- bash -c 'walk_story_stack; exit $?' || status=$?
      if [[ ! -f $out/walked ]]; then
        echo "FAIL: $story: its stack could not be given a network namespace of its own (unshare --user --map-root-user --mount --net exited $status)"
        printf '{"id": "%s", "at": "%s", "seconds": 0, "completed": false, "withinBudget": false, "brokeAt": null, "untested": "its stack could not be given a network namespace of its own", "steps": [], "timings": {}, "videos": []}\n' \
          "$story" "$(date -u +%FT%TZ)" > "$out/story.json"
      fi
    else
      walk_story_stack || status=$?
    fi
    # The stack's logs stay with the story; its state does not.
    mkdir -p "$out/logs"
    cp "$out/state"/logs/*.log "$out/logs/" 2>/dev/null || true
    rm -rf "$out/state"
  fi
  if [[ -n $serving ]]; then
    # What the Nextcloud said while the story was walked stays with the
    # recording: a person meeting it is the one thing no shell log holds.
    mkdir -p "$out/logs"
    podman logs tilo-cloud-conformance-nextcloud > "$out/logs/nextcloud.log" 2>&1 || true
    stop_servers
  fi
  python3 -I "$stories/judge.py" "$out" || echo "FAIL: $story: the judge did not answer"
  # The model's verdict: 1 accepted (or nothing on screen to judge), 0 rejected, 2 no model
  # looked. Only a rejection makes a story whose steps passed red.
  verdict=$(python3 -I -c 'import json, sys; j = json.load(open(sys.argv[1])); print(1 if j.get("accept") and j.get("judged", True) is not False or j.get("applicable") is False else 2 if not j.get("judged") else 0)' "$out/judge.json" 2>/dev/null || echo 2)
  if ((status == 0 && verdict == 1)); then
    echo "GREEN $story"
  elif ((status == 0 && verdict == 2)); then
    echo "PASSED, NOT JUDGED $story"
  else
    echo "RED $story"
    red+=("$story")
  fi
done

python3 -I "$stories/page.py" "$work"
if ((${#red[@]})); then
  echo "FAIL: red: ${red[*]}"
  exit 1
fi
echo "ok: every story walked is green (${asked[*]})"
