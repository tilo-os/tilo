#!/usr/bin/env bash
# The Lounge image's build context (os/containers/tilo-lounge): the programs
# (lounge-binaries.sh), the bot programs the workspace installs by path, and
# the product font catalog its app components lay text out from, since a
# container has no system fonts. usage: lounge-context.sh OUT BINARIES FONTS
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: lounge-context.sh OUT BINARIES FONTS'
out=${1:?$usage}
binaries=${2:?$usage}
fonts=${3:?$usage}
cd "$repo_root"
[[ -x $binaries/tilo-relay && -x $binaries/discord-worker ]] || die "no Lounge programs in $binaries"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"
rm -rf "$out"
mkdir -p "$out/bin" "$out/bots" "$out/fonts"
cp "$binaries/tilo-relay" "$binaries/discord-worker" "$out/bin/"
cp -a "$fonts/." "$out/fonts/"
cp apps/discord/bridge/program.json "$out/bots/discord-bridge.json"
# Every version of each built-in app's surface.toml the apps' history holds, one
# file per version: the relay publishes them into its content plane at start, so
# a change written by an older version than this build runs is decided without
# asking anybody. Published from the apps' history, so the checkout needs it.
if [[ $(git rev-parse --is-shallow-repository) == true ]]; then
  die 'the Lounge publishes its app surfaces from the apps history: a shallow checkout has none of it'
fi
mkdir -p "$out/surfaces"
for surface in apps/*/surface.toml; do
  app=${surface#apps/}
  app=${app%/surface.toml}
  while read -r commit; do
    git cat-file -p "$commit:$surface" > "$out/surfaces/$app.$commit.toml"
  done < <(git log --format=%H -- "$surface")
done
[[ $(ls "$out/surfaces" | wc -l) -gt 0 ]] \
  || die 'the app surfaces are not in the checkout: the Lounge would read its history with the newest of them alone'
echo "surfaces: $(ls "$out/surfaces" | wc -l) versions of the built-in apps' surface.toml"
container=os/containers/tilo-lounge
cp "$container/Containerfile" "$container/Containerfile.dockerignore" "$container/entrypoint.sh" \
  os/scripts/verify-no-cisco-openh264.sh "$out/"
# The image's revision label and TILO_SOURCE_REVISION: the commit this context
# was built from, which GET /mode reports.
git rev-parse HEAD > "$out/source-revision"
