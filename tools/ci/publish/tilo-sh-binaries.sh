#!/usr/bin/env bash
# What t's checks of the local tilo.sh stack run natively, built once for both
# of them: the release's code and opt-level without the whole-program link
# (Cargo.toml's `lounge-check`), and the server's laid-out context, which reads
# the apps' history for its surfaces (lounge-context.sh) and carries the product
# font catalog, and the apps its workspace runs. usage: tilo-sh-binaries.sh OUT
# LOUNGE_BINARIES FONTS APPS
#
#   OUT           the stack's own programs in bin/ (TILO_SH_PROGRAMS) and its
#                 lounge/ (TILO_SH_LOUNGE)
#   LOUNGE_BINARIES  lounge-check-binaries' output: tilo-runtime and
#                 discord-worker, built as the Lounge image ships them
#   FONTS         lounge-fonts' output
#   APPS          lounge-check-apps' output: the launcher, the runtime bundles,
#                 the key they and the packages are trusted by, and the packages
#                 the workspace installs
#
# The server is not built again here: its two programs are the ones the Lounge's
# page checks run, and they are the same build the image's context takes.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: tilo-sh-binaries.sh OUT LOUNGE_BINARIES FONTS APPS'
out=${1:?$usage}
binaries=${2:?$usage}
fonts=${3:?$usage}
apps=${4:?$usage}
[[ -x $binaries/tilo-runtime && -x $binaries/discord-worker ]] || die "no Lounge programs in $binaries"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out/bin"
# The API server starts a task's tilolang programs in tilolang-process, which it finds beside
# itself: one build, one directory. The VM host's start reads the server's endpoint id from the
# host info the daemon keeps (`web-host.json`).
cargo build --locked --profile lounge-check -p tilo-api -p fake-discord -p tilolang-process \
  -p tilo-account-probe --example mail_stand_in \
  --bin tilo-api --bin fake-discord --bin tilolang-process
# A device starts every app through the launcher beside its programs, and the
# flows' devices (the `tilo` command's) are given the Lounge's app layer: their
# launcher is the one a check builds, which reads the trust list they are given
# rather than the image's /etc/tilo.
cargo build --locked --profile lounge-check -p launcher --features launcher/dev-overrides \
  --bin tilo-launcher
for bin in tilo-api fake-discord tilolang-process tilo-launcher; do
  cp "$target/lounge-check/$bin" "$out/bin/"
done
# The stand-in mail server is an example, since it makes a certificate with the
# crate's test-time dependencies; the stack starts it as mail-stand-in.
cp "$target/lounge-check/examples/mail_stand_in" "$out/bin/mail-stand-in"
bash tools/ci/publish/lounge-context.sh "$out/lounge" "$binaries" "$fonts" "$apps"
# Outside the image a Tilo program finds the font catalog in fonts/ beside itself,
# and the stack reads this layout as it is (in a check, read-only).
ln -sfn ../fonts "$out/lounge/bin/fonts"
echo "the stack's programs: $out/bin; its server's context: $out/lounge"
