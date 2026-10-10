#!/usr/bin/env bash
# The whole path of an app developer who has rustup and nothing from Tilo:
# one `cargo install`, then `tilo-app new`, `check` and `package` outside any
# checkout. Run it where no GUI development package is installed; that is
# what it proves.
#
#   tools/ci/standalone-app.sh [--bootstrap] [--git URL] [--branch NAME | --rev SHA]
#
# --bootstrap first installs gcc and rustup's default profile into a bare
# Fedora, and a font once `check` has asked for one. That is how
# .forgejo/workflows/standalone-app.yml runs it:
#
#   podman run --rm -i registry.fedoraproject.org/fedora:44 \
#     bash -s -- --bootstrap --branch main < tools/ci/standalone-app.sh
set -euo pipefail

git_url=https://code.tilo.so/tilo/tilo.git
pin=()
work=
bootstrap=0
while [ $# -gt 0 ]; do
  case $1 in
    --bootstrap) bootstrap=1; shift ;;
    --git) git_url=$2; shift 2 ;;
    --branch|--rev) pin=("$1" "$2"); shift 2 ;;
    --work) work=$2; shift 2 ;;
    *) echo "usage: $0 [--bootstrap] [--git URL] [--branch NAME | --rev SHA] [--work DIR]" >&2; exit 2 ;;
  esac
done

step() { printf '\n==> %s\n' "$*"; }

if [ "$bootstrap" = 1 ]; then
  step "gcc and rustup, nothing else"
  dnf -y -q install gcc
  # The checksum-pinned rustup of dist/ci/install-build-deps.sh.
  rustup_version=1.29.0
  case "$(uname -m)" in
    x86_64) rustup_sha=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10 ;;
    aarch64) rustup_sha=9732d6c5e2a098d3521fca8145d826ae0aaa067ef2385ead08e6feac88fa5792 ;;
    *) echo "no pinned rustup for $(uname -m)" >&2; exit 1 ;;
  esac
  # rustup-init picks its mode from its own file name.
  rustup_init=$(mktemp -d)/rustup-init
  curl --proto '=https' --tlsv1.2 -fsSL -o "$rustup_init" \
    "https://static.rust-lang.org/rustup/archive/$rustup_version/$(uname -m)-unknown-linux-gnu/rustup-init"
  printf '%s  %s\n' "$rustup_sha" "$rustup_init" | sha256sum --check --status
  chmod 0755 "$rustup_init"
  "$rustup_init" -y --no-modify-path --profile default
  rm -rf "${rustup_init%/*}"
  export PATH="$HOME/.cargo/bin:$PATH"
fi

work=${work:-$(mktemp -d)}
mkdir -p "$work"
cd "$work"

# A check that cannot observe its subject must fail: with these present the
# build would pass whether or not the app graph still needs them.
step "no GUI development libraries"
if command -v pkg-config >/dev/null 2>&1; then
  for module in xkbcommon wayland-client fontconfig x11 gstreamer-1.0; do
    if pkg-config --exists "$module"; then
      echo "pkg-config finds $module: this environment cannot show that apps build without it" >&2
      exit 1
    fi
  done
fi
if git -C "$work" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "$work is inside a git work tree; an app outside a checkout must not be" >&2
  exit 1
fi
echo "none of xkbcommon, wayland-client, fontconfig, x11, gstreamer-1.0"

step "cargo install --locked --git $git_url ${pin[*]} tilo-app-cli"
cargo install --locked --root "$work/cli" --git "$git_url" "${pin[@]}" tilo-app-cli
export PATH="$work/cli/bin:$PATH"

step "tilo-app new demo"
tilo-app new demo --id org.example.demo
cd demo
# The SDK must be the commit the CLI came from, not whatever main is now.
if ! grep -E '^ui = .*rev = "[0-9a-f]{40}"' Cargo.toml; then
  echo "the app's SDK is not pinned to the commit tilo-app was installed from:" >&2
  grep -E '^ui = ' Cargo.toml >&2
  exit 1
fi

# Fonts are the one runtime need a bare container lacks: `check` must name
# the package rather than fail inside the text shaper.
if ! compgen -G "/usr/share/fonts/*" >/dev/null; then
  step "tilo-app check names the font a bare machine lacks"
  if tilo-app check >fonts.log 2>&1; then
    cat fonts.log >&2
    echo "check passed on a machine with no fonts" >&2
    exit 1
  fi
  if ! grep -F "google-noto-sans-fonts" fonts.log; then
    cat fonts.log >&2
    echo "check failed without naming the missing font package" >&2
    exit 1
  fi
  [ "$bootstrap" = 1 ] || { echo "install a font to go on" >&2; exit 1; }
  dnf -y -q install google-noto-sans-fonts
fi

step "tilo-app check"
tilo-app check

step "tilo-app package --platform linux"
tilo-app package --platform linux
ls -l dist

# The one library an app cannot avoid: a thin Tilo OS package links the
# toolkit the OS ships, and that links GStreamer. It must say so up front.
step "tilo-app package names what a thin build needs"
if tilo-app package --force >thin.log 2>&1; then
  echo "a thin package built without GStreamer's development files" >&2
  exit 1
fi
grep -F "gstreamer1-devel" thin.log
grep -F "libgstreamer1.0-dev" thin.log

printf '\nstandalone app OK\n'
