#!/bin/bash
# An MCP client standing in for Claude Code, run in a Terminal environment's shell
# (scripts/vm-acceptance/acceptance/checks/agents_e2e.py).
#
#     bash mcp_agent.sh DIR [SERVER ARGS...]
#
# It starts the MCP server the way Claude Code starts the one a person configured as
# {"command": "tilo-runtime", "args": ["--connect"]} (SERVER ARGS default to that), and
# speaks newline-delimited JSON-RPC over its stdio. What to send comes from the harness, a
# request per line appended to DIR/in (`quit` ends the conversation), standing in for the
# model's decisions; every line the server writes is appended to DIR/out, and the server's
# stderr to DIR/err. DIR/hello says where it runs; DIR/done that it is over, with the
# server's exit status. It judges nothing: the harness reads DIR/out.
#
# The environment's runtime has bash and coreutils, and no python: this is bash alone.
set -u
dir=$1
shift
[ $# -gt 0 ] || set -- tilo-runtime --connect
mkdir -p "$dir"
: > "$dir/out"
: > "$dir/err"
touch "$dir/in"
rm -f "$dir/done"
pipes=$(mktemp -d)
mkfifo "$pipes/to" "$pipes/from"
"$@" < "$pipes/to" > "$pipes/from" 2>> "$dir/err" &
server=$!
exec 3> "$pipes/to"
{
  while IFS= read -r line; do
    printf '%s\n' "$line" >> "$dir/out"
  done < "$pipes/from"
  printf '%s\n' '{"e2e":"eof"}' >> "$dir/out"
} 3>&- &
reader=$!
printf '{"pid":%s,"server":%s,"environment":"%s","workspace":"%s","agent_sock":"%s","home":"%s","command":"%s"}\n' \
  "$$" "$server" "${TILO_ENVIRONMENT:-}" "${TILO_WORKSPACE:-}" "${TILO_AGENT_SOCK:-}" "$HOME" "$*" > "$dir/hello"
sent=0
while kill -0 "$server" 2>/dev/null; do
  total=$(wc -l < "$dir/in")
  while [ "$sent" -lt "$total" ]; do
    sent=$((sent + 1))
    line=$(sed -n "${sent}p" "$dir/in")
    [ "$line" = quit ] && break 2
    printf '%s\n' "$line" >&3
  done
  sleep 0.2
done
# Closing stdin ends the server, as Claude Code ending does.
exec 3>&-
status=0
wait "$server" || status=$?
wait "$reader"
rm -rf "$pipes"
printf '{"status":%s}\n' "$status" > "$dir/done"
