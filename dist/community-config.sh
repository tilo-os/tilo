#!/usr/bin/env bash
# Check, and optionally materialize, the community configuration a build ships.
#
# The shell reads /usr/share/tilo/community.toml (see
# crates/session/shell/src/shell/community.rs): the Tilo Community room's
# standing invitation and the Tilo team principal. The OS image and every
# desktop-session package bake it from os/usr/share/tilo/community.toml, or
# from the TILO_COMMUNITY_TOML text CI provides. A release must carry a real
# one; a development build may ship the empty placeholder.
#
# Plain bash and coreutils only: this runs in every package userspace
# (Fedora, Ubuntu, Debian, Arch, Nix) and inside os/Containerfile.
set -euo pipefail

prog=${0##*/}

usage() {
    cat <<EOF
usage: $prog check [--require] FILE
       $prog materialize [--require] DEST

check        Validate FILE against the shape the shell reads:
                 [community]
                 invite = "<standing invitation>"
                 [team]
                 user_public_key = "<64 hex>"
                 label = "Tilo team"
             Unknown sections or keys are errors, as they are to the shell.
             Without --require, empty values pass (an unconfigured
             development build); values that are set must be well formed.
             With --require (a release), the invitation must be a standing
             invitation and the team key 64 hex digits, not all zero.

materialize  When TILO_COMMUNITY_TOML is set and non-empty (the CI-provided
             file, e.g. a Forgejo repository variable), write it over DEST;
             then check DEST as above.

A release build sets TILO_REQUIRE_COMMUNITY=1, which the build scripts turn
into --require.
EOF
}

fix="run \`scripts/community-ops/tilo-community up\` and commit os/usr/share/tilo/community.toml, or set the TILO_COMMUNITY_TOML repository variable"

die() {
    printf '%s: %s\n' "$prog" "$1" >&2
    exit 1
}

refuse() {
    printf '%s: %s\n  fix: %s\n' "$prog" "$1" "$fix" >&2
    exit 1
}

# Print "section<TAB>key<TAB>value" for every assignment, or fail naming the
# first line the shell would refuse.
parse() {
    awk '
        function trim(s) { sub(/^[[:space:]]+/, "", s); sub(/[[:space:]]+$/, "", s); return s }
        {
            line = $0
            sub(/^[[:space:]]+/, "", line)
            if (line == "" || substr(line, 1, 1) == "#") next
            if (substr(line, 1, 1) == "[") {
                if (!match(line, /^\[[[:space:]]*[A-Za-z_]+[[:space:]]*\][[:space:]]*(#.*)?$/)) {
                    printf "line %d: not a section header: %s\n", NR, $0 > "/dev/stderr"; bad = 1; exit
                }
                section = line
                sub(/^\[[[:space:]]*/, "", section); sub(/[[:space:]]*\].*$/, "", section)
                if (section != "community" && section != "team") {
                    printf "line %d: unknown section [%s]\n", NR, section > "/dev/stderr"; bad = 1; exit
                }
                next
            }
            if (!match(line, /^[A-Za-z_]+[[:space:]]*=[[:space:]]*"[^"\\]*"[[:space:]]*(#.*)?$/)) {
                printf "line %d: not a key = \"string\" assignment: %s\n", NR, $0 > "/dev/stderr"; bad = 1; exit
            }
            key = line; sub(/[[:space:]]*=.*$/, "", key)
            value = line; sub(/^[^"]*"/, "", value); sub(/".*$/, "", value)
            allowed = (section == "community" && key == "invite") \
                   || (section == "team" && (key == "user_public_key" || key == "label"))
            if (!allowed) {
                printf "line %d: unknown key %s%s\n", NR, (section == "" ? "" : section "."), key > "/dev/stderr"; bad = 1; exit
            }
            if (seen[section "." key]++) {
                printf "line %d: %s.%s is set twice\n", NR, section, key > "/dev/stderr"; bad = 1; exit
            }
            printf "%s\t%s\t%s\n", section, key, trim(value)
        }
        END { exit bad }
    ' "$1"
}

# Whether $1 is URL-safe base64 (no padding) of JSON carrying "standing": true.
standing_invite() {
    local token=$1 padded json
    [[ $token =~ ^[A-Za-z0-9_-]+$ ]] || return 1
    padded=$(printf '%s' "$token" | tr '_-' '/+')
    while (( ${#padded} % 4 )); do padded+='='; done
    json=$(printf '%s' "$padded" | base64 -d 2>/dev/null | tr -d '[:space:]') || return 1
    [[ $json == \{* && $json == *'"standing":true'* ]]
}

check() {
    local require=$1 file=$2 parsed invite="" key="" section name value
    [[ -f $file ]] || die "$file does not exist; the build must ship a community configuration (the empty placeholder for a development build)"
    parsed=$(parse "$file") || die "$file is not the community configuration the shell reads"
    while IFS=$'\t' read -r section name value; do
        [[ -n $section ]] || continue
        case "$section.$name" in
            community.invite) invite=$value ;;
            team.user_public_key) key=$value ;;
        esac
    done <<<"$parsed"

    if [[ -n $invite ]] && ! standing_invite "$invite"; then
        refuse "$file: community.invite is not a standing invitation (URL-safe base64 of a signed token with \"standing\": true)"
    fi
    if [[ -n $key ]]; then
        [[ $key =~ ^[0-9a-fA-F]{64}$ ]] \
            || refuse "$file: team.user_public_key must be 32 bytes of hex (64 digits)"
        [[ ! $key =~ ^0{64}$ ]] \
            || refuse "$file: team.user_public_key is all zeros, not a team key"
    fi

    if [[ -n $invite && -n $key ]]; then
        echo "$prog: $file names the community invitation and the team principal"
        return 0
    fi
    if (( require )); then
        [[ -n $invite ]] || refuse "$file: a release needs community.invite, the Tilo Community room's standing invitation"
        refuse "$file: a release needs team.user_public_key, the Tilo team's user root key"
    fi
    echo "$prog: $file is unconfigured: dev build (no community invitation or team principal ships)"
}

require=0
command=${1:-}
[[ $# -gt 0 ]] && shift
case "$command" in
    -h|--help|help) usage; exit 0 ;;
    check|materialize) ;;
    *) usage >&2; exit 2 ;;
esac
if [[ ${1:-} == --require ]]; then
    require=1
    shift
fi
[[ $# == 1 ]] || { usage >&2; exit 2; }
target=$1

if [[ $command == materialize && -n ${TILO_COMMUNITY_TOML:-} ]]; then
    printf '%s\n' "$TILO_COMMUNITY_TOML" > "$target"
    echo "$prog: wrote TILO_COMMUNITY_TOML to $target"
fi
check "$require" "$target"
