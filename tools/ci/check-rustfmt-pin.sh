#!/usr/bin/env bash
# The rustfmt a commit or push formats with must be the one `rust-toolchain.toml` pins: a
# different version lays code out its own way, and the tree's `fmt` check (which runs the
# pinned one) rewrites it again. Run from a hook: exits 1, with what to do, when rustfmt is
# missing or is not the pinned toolchain's.
#
#   tools/ci/check-rustfmt-pin.sh
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
channel=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$root/rust-toolchain.toml" 2>/dev/null | head -n 1)
if [ -z "$channel" ]; then
  echo "rust-toolchain.toml names no channel to pin rustfmt to" >&2
  exit 1
fi
# A machine with no rustfmt of its own formats through t (`t test fmt`, `t fix fmt`, the
# pinned toolchain): there is no local version to pin, so this passes and the caller uses
# that path instead.
if ! command -v rustfmt >/dev/null 2>&1; then
  exit 0
fi
pinned=""
for dir in "$HOME"/.rustup/toolchains/"$channel"-*; do
  [ -x "$dir/bin/rustfmt" ] || continue
  pinned=$(readlink -f "$dir/bin/rustfmt")
  break
done
if [ -z "$pinned" ]; then
  echo "the $channel toolchain's rustfmt is not installed under $HOME/.rustup: install it (rustup toolchain install $channel) or format through 't fix fmt'" >&2
  exit 1
fi
found=$(readlink -f "$(command -v rustfmt)")
if [ "$found" != "$pinned" ] && [ "$(rustfmt --version)" != "$("$pinned" --version)" ]; then
  echo "rustfmt on PATH is $(rustfmt --version) ($found), not the $channel toolchain's $("$pinned" --version) ($pinned): format through 't fix fmt', or put the pinned toolchain's bin first on PATH" >&2
  exit 1
fi
