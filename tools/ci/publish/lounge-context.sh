#!/usr/bin/env bash
# The Lounge image's build context (os/containers/tilo-lounge): the programs
# (lounge-binaries.sh), the product font catalog its app components lay text out
# from, since a container has no system fonts, and the apps its workspace runs
# (lounge-apps.sh). usage: lounge-context.sh OUT BINARIES FONTS APPS
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: lounge-context.sh OUT BINARIES FONTS APPS'
out=${1:?$usage}
binaries=${2:?$usage}
fonts=${3:?$usage}
apps=${4:?$usage}
cd "$repo_root"
[[ -x $binaries/tilo-runtime && -x $binaries/discord-worker ]] || die "no Lounge programs in $binaries"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"
[[ -x $apps/bin/tilo-launcher && -d $apps/runtimes && -d $apps/packages ]] || die "no Lounge apps in $apps"
rm -rf "$out"
mkdir -p "$out/bin" "$out/fonts"
cp "$binaries/tilo-runtime" "$binaries/discord-worker" \
  "$apps/bin/tilo-launcher" "$apps/bin/tilolang-process" "$apps/bin/tilo-install" "$out/bin/"
cp -a "$apps/runtimes" "$apps/packages" "$apps/runtime-publishers.txt" "$out/"
cp -a "$fonts/." "$out/fonts/"
# Every version of each built-in app's compiled surface the apps' history holds, one
# file per version: the server publishes them into its content store at start, so
# a change written by an older version than this build runs is decided without
# asking anybody.
#
# A checkout without git (a build server's mirror) has no history: it gives the
# current version of each surface and TILO_LOUNGE_SURFACE_COMMIT names the
# commit it is from. The server publishes those, and decides a change written by
# an older version with a member's help instead of alone.
mkdir -p "$out/surfaces"
surfaces_from_history() {
  # Each app's history is walked on its own, all at once: one after the other
  # they took fourteen seconds of every local bring-up.
  local -a pids=()
  surface_versions() {
    local surface=$1 app commit
    app=${surface#apps/}
    app=${app%/surface.json}
    while read -r commit; do
      git cat-file -p "$commit:$surface" > "$out/surfaces/$app.$commit.json"
    done < <(git log --format=%H -- "$surface")
  }
  for surface in apps/*/surface.json; do
    surface_versions "$surface" &
    pids+=("$!")
  done
  for pid in "${pids[@]}"; do wait "$pid" || die 'could not read the app surfaces from the apps history'; done
  [[ $(ls "$out/surfaces" | wc -l) -gt 0 ]] \
    || die 'the app surfaces are not in the checkout: the Lounge would read its history with the newest of them alone'
}
surfaces_from_checkout() {
  local surface app
  for surface in apps/*/surface.json; do
    app=${surface#apps/}
    app=${app%/surface.json}
    cp "$surface" "$out/surfaces/$app.${TILO_LOUNGE_SURFACE_COMMIT:-current}.json"
  done
}
if git rev-parse --show-toplevel >/dev/null 2>&1; then
  if [[ $(git rev-parse --is-shallow-repository) == true ]]; then
    die 'the Lounge publishes its app surfaces from the apps history: a shallow checkout has none of it'
  fi
  surfaces_from_history
else
  surfaces_from_checkout
fi
echo "surfaces: $(ls "$out/surfaces" | wc -l) versions of the built-in apps' compiled surfaces"
container=os/containers/tilo-lounge
cp "$container/Containerfile" "$container/Containerfile.dockerignore" "$container/entrypoint.sh" \
  "$container/install-apps.sh" os/scripts/verify-no-cisco-openh264.sh "$out/"
# The image's revision label and TILO_SOURCE_REVISION: the commit this context
# was built from, which GET /mode reports.
printf '%s\n' "${TILO_LOUNGE_SURFACE_COMMIT:-$(git rev-parse HEAD)}" > "$out/source-revision"
