#!/usr/bin/env bash
# A fresh tilo.sh stack answers an agent's program. The stack comes up here from the one build
# its programs are made in (the check tilo-sh-programs: the server, the API server, the tilo
# command, and tilolang-process, which the API server starts a task's tilolang programs in and
# looks for beside itself), and one small program goes to the API server's
# POST /v1/workspaces/{workspace}/programs with the key of the stack's own person, the Lounge's
# first owner (stack.sh owner --here: no browser). The answer must be `ok`. On a build without
# tilolang-process beside tilo-api the same call is answered that none is installed, and no
# other check of the stack sends a program: this one fails there, in the API server's words.
#
# usage: test-program.sh WORK
#
# WORK keeps the stack's state (WORK/state) and the API server's answer (WORK/answer.json).
# From the environment: TILO_SH_PROGRAMS (tilo-sh-programs' output), TILO_SH_CADDY
# (tilo-sh-caddy's) and TILO_SH_FONTS (lounge-fonts' catalog, which the stack lays the server's
# context out with); bwrap, curl, python3 and openssl, as every stack here needs. The site is a
# stub of the two files the web entry starts with: nothing opens a page.
set -euo pipefail
usage='usage: test-program.sh WORK'
work=${1:?$usage}
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
die() { printf 'test-program: %s\n' "$*" >&2; exit 1; }
programs=${TILO_SH_PROGRAMS:?TILO_SH_PROGRAMS names the programs the check tilo-sh-programs built}
fonts=${TILO_SH_FONTS:?TILO_SH_FONTS names the font catalog the check lounge-fonts built}
: "${TILO_SH_CADDY:?TILO_SH_CADDY names the Caddy the check tilo-sh-caddy fetched}"
[[ -x $programs/tilo ]] || die "$programs holds no tilo command, which the stack's own person acts through"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"

mkdir -p "$work"
work=$(cd -- "$work" && pwd)
export TILO_SH_STATE=$work/state TILO_SH_REMOTE=0 TILO_SH_TILO_BIN=$programs/tilo
export TILO_SH_SITE=$work/site
stack() { bash "$here/stack.sh" "$@"; }
trap 'stack down >/dev/null 2>&1 || true' EXIT
# A state of its own, new, with the font catalog where the stack looks before it fetches one.
stack down --wipe >/dev/null 2>&1 || true
rm -rf "$TILO_SH_SITE" "$work/answer.json"
mkdir -p "$TILO_SH_SITE/site" "$TILO_SH_STATE/native"
printf '<!doctype html><title>tilo.sh</title>\n' > "$TILO_SH_SITE/site/index.html"
cp "$repo/dist/install/tilo.sh" "$TILO_SH_SITE/install.sh"
cp -r "$fonts" "$TILO_SH_STATE/native/fonts"
# What up prints on its output is an owner's link, which nobody here opens.
stack up >/dev/null || die 'the stack did not come up'

# The key of the stack's own person, read into this shell and never printed.
stack owner --here >/dev/null 2>&1 || die 'the stack made its owner no key (stack.sh owner --here)'
eval "$(stack env)"
[[ -n ${TILO_API_KEY:-} && -n ${TILO_API_URL:-} ]] || die 'stack.sh env named no API server and key'
workspace=$(cat "$TILO_SH_STATE/server/lounge/workspace-id") || die 'the server kept no workspace id'

# One program, which reads the notebook Notes keeps in the Lounge and changes nothing. The key
# goes to curl on its input, not among its arguments.
answer=$work/answer.json
status=$(curl -sS -o "$answer" -w '%{http_code}' --max-time 120 \
  -H @- -H 'content-type: application/json' -d '{"program":"notes.notebook"}' \
  "$TILO_API_URL/v1/workspaces/$workspace/programs" <<< "Authorization: Bearer $TILO_API_KEY") \
  || die 'the API server did not answer the program'
[[ $status == 200 ]] || die "the API server answered $status to the program: $(head -c 2000 "$answer")"
said=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1])).get("status", ""))' "$answer") \
  || die "the API server's answer is no JSON: $(head -c 2000 "$answer")"
[[ $said == ok ]] || die "the program's answer is ${said:-no status}, not ok: $(head -c 2000 "$answer")"
echo "the stack answered one program from tilolang-process: $(head -c 400 "$answer")"
