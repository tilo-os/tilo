#!/usr/bin/env bash
# tilolang as a person gets it on a computer that has none of it yet, run by t's
# installer-clean-{fedora,debian} checks in a stock distribution image (`t dispatch
# installer-clean-fedora --ref main --repo tilo`, `--input RELEASE=vX.Y.Z` for a release other than
# the one tools/ci/publish/release-version names):
#
#   1. `curl -fsSL https://tilolang.dev/install | sh` installs the release, and
#      `tilolang --version` names it;
#   2. `tilolang new` starts an app, and `tilolang check` on a computer without Rust says to
#      install it with rustup;
#   3. with Rust installed by rustup, `tilolang check` and `tilolang build` pass;
#   4. `cargo install tilolang-cli` from crates.io, into an empty CARGO_HOME, installs the same
#      release, whose `new`, `check` and `build` pass too.
#
# Every step runs, and each one that failed is named at the end, so one run shows all of what a
# person would hit.
set -uo pipefail

release=${T_INPUT_RELEASE:-}
[[ -n $release ]] || release=v$(tr -d '[:space:]' < tools/ci/publish/release-version)
version=${release#v}
# The image's own user, in the home the image gave it, which is empty, as a new computer's is:
# rustup refuses a HOME other than the user's own.
HOME=$(getent passwd "$(id -u)" | cut -d: -f6)
export HOME
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
export PATH=$HOME/.local/bin:$PATH
failed=()

step() {
  local name=$1
  shift
  printf '== %s\n' "$name"
  if "$@"; then
    printf 'ok: %s\n' "$name"
  else
    printf 'FAILED: %s\n' "$name"
    failed+=("$name")
  fi
}

names_release() {
  local said
  said=$("$1" --version) || return 1
  printf '%s\n' "$said"
  [[ $said == "tilolang $version" ]]
}

starts_app() {
  mkdir -p "$1" && (cd "$1" && "$2" new hello)
}

says_install_rust() {
  local out
  if out=$(cd "$1/hello" && "$2" check 2>&1); then
    printf '%s\n' "$out"
    echo "check passed on a computer without Rust" >&2
    return 1
  fi
  printf '%s\n' "$out"
  grep -q 'https://rustup.rs' <<<"$out"
}

checks_and_builds() {
  (cd "$1/hello" && "$2" check && "$2" build)
}

install_rust() {
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
    && . "$HOME/.cargo/env"
}

cargo_install() {
  CARGO_HOME=$work/cargo-home cargo install tilolang-cli --version "$version"
}

step "the installer installs $release" \
  sh -c "curl -fsSL https://tilolang.dev/install | TILO_VERSION=$release sh"
step "the installed tilolang names $version" names_release "$HOME/.local/bin/tilolang"
step "tilolang new starts an app" starts_app "$work/installed" "$HOME/.local/bin/tilolang"
step "tilolang check without Rust says to install it" \
  says_install_rust "$work/installed" "$HOME/.local/bin/tilolang"
step "rustup installs Rust" install_rust
step "tilolang check and build pass with Rust" \
  checks_and_builds "$work/installed" "$HOME/.local/bin/tilolang"
step "cargo install tilolang-cli $version into an empty CARGO_HOME" cargo_install
cargo_tilolang=$work/cargo-home/bin/tilolang
step "cargo's tilolang names $version" names_release "$cargo_tilolang"
step "cargo's tilolang new starts an app" starts_app "$work/cargo" "$cargo_tilolang"
step "cargo's tilolang check and build pass" checks_and_builds "$work/cargo" "$cargo_tilolang"

if ((${#failed[@]})); then
  printf 'tilolang %s as a person installs it: %d step(s) failed:\n' "$version" "${#failed[@]}"
  printf '  %s\n' "${failed[@]}"
  exit 1
fi
printf 'tilolang %s installs, starts, checks and builds an app, from the installer and from crates.io\n' \
  "$version"
