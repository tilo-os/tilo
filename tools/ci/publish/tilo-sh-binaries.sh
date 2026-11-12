#!/usr/bin/env bash
# What t's checks of the local tilo.sh stack run natively, built once for both
# of them: the release's code and opt-level without the whole-program link
# (Cargo.toml's `lounge-gate`), and the relay's laid-out context, which reads
# the apps' history for its surfaces (lounge-context.sh) and carries the product
# font catalog. usage: tilo-sh-binaries.sh OUT LOUNGE_BINARIES FONTS
#
#   OUT           the stack's own programs in bin/ (TILO_SH_PROGRAMS) and its
#                 lounge/ (TILO_SH_LOUNGE)
#   LOUNGE_BINARIES  lounge-gate-binaries' output: tilo-relay and
#                 discord-worker, built as the Lounge image ships them
#   FONTS         lounge-fonts' output
#
# The relay is not built again here: its two programs are the ones the Lounge's
# page checks run, and they are the same build the image's context takes.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: tilo-sh-binaries.sh OUT LOUNGE_BINARIES FONTS'
out=${1:?$usage}
binaries=${2:?$usage}
fonts=${3:?$usage}
[[ -x $binaries/tilo-relay && -x $binaries/discord-worker ]] || die "no Lounge programs in $binaries"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out/bin"
cargo build --locked --profile lounge-gate -p tilo-front-door -p fake-discord \
  --bin tilo-front-door --bin fake-discord
for bin in tilo-front-door fake-discord; do cp "$target/lounge-gate/$bin" "$out/bin/"; done
bash tools/ci/publish/lounge-context.sh "$out/lounge" "$binaries" "$fonts"
# Outside the image a Tilo program finds the font catalog in fonts/ beside itself,
# and the stack reads this layout as it is (in a check, read-only).
ln -sfn ../fonts "$out/lounge/bin/fonts"
echo "the stack's programs: $out/bin; its relay's context: $out/lounge"
