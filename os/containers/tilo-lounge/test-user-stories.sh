#!/usr/bin/env bash
# The user stories (stories/stories.json), walked as the people in them would:
# each on a fresh tilo.sh stack of its own (tilo-sh/stack.sh, production's
# entrypoints and environment over the programs t built, as test-e2e.sh brings
# it up), its people in fresh browser profiles, every step recorded, timed and
# held to its budget (stories/recorder.mjs). Then a first-time user's verdict on
# each recording (stories/judge.py, a model with no knowledge of the code), and
# one page of every story, green or red (stories/page.py).
#
# usage: test-user-stories.sh WORK [STORY…]
# STORY names the stories to walk (default: every one with a driver). WORK
# keeps each story's recording in WORK/<story>/ (story.json, judge.json, the
# step screenshots, each person's video, a failing step's console and the
# services' log) with the stack's logs in WORK/<story>/logs/, and the page in
# WORK/index.html, every link in it relative to WORK.
#
# Needs node, chromium, curl, bwrap, ffmpeg (the videos), the stack's
# environment (TILO_SH_PROGRAMS, TILO_SH_LOUNGE, TILO_SH_SITE, TILO_SH_CADDY
# naming what t built), TILO_CLI_BIN, and ANTHROPIC_API_KEY and the anthropic
# Python package for the judge (without them a story whose steps pass is
# "passed, not judged": never green, and not red for that alone). A story marked `"stack": false` needs none of the stack's,
# only what its driver says it does. Exits non-zero when any story walked is red.
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
# Whether a story is walked on a stack of its own (all are, but those marked `"stack": false`).
on_stack() { python3 -I -c 'import json, sys; sys.exit(0 if next(s for s in json.load(open(sys.argv[1]))["stories"] if s["id"] == sys.argv[2]).get("stack", True) else 1)' "$stories/stories.json" "$1"; }
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
    STORY_STACK=$tilo_sh/stack.sh node "$stories/$driver" "$site_origin" "$out" "$TILO_SH_STATE"/logs/{server,api,web}.log 2>&1 | tee "$out/driver.log" || status=$?
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
for story in "${asked[@]}"; do
  out=$work/$story
  rm -rf "$out"
  mkdir -p "$out"
  driver=$(python3 -I -c 'import json, sys; print(next(s["driver"] for s in json.load(open(sys.argv[1]))["stories"] if s["id"] == sys.argv[2]))' "$stories/stories.json" "$story")
  { read -r TILO_SH_ADMISSION; read -r TILO_SH_MAIL_STAND_IN; read -r private_network; } < <(stacks_for_story "$story")
  export TILO_SH_ADMISSION TILO_SH_MAIL_STAND_IN private_network
  if ! on_stack "$story"; then
    # A story with no stack walks what it drives itself (its driver says what).
    status=0
    node "$stories/$driver" "" "$out" 2>&1 | tee "$out/driver.log" || status=$?
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
        printf '{"id": "%s", "at": "%s", "seconds": 0, "completed": false, "withinBudget": false, "brokeAt": null, "error": "the stack could not be given a network namespace of its own", "steps": [], "timings": {}, "videos": []}\n' \
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
