#!/bin/sh
# Install the `tilo` command, as `curl -fsSL https://tilo.sh | sh` runs it.
#
# It needs no root: the command goes to ~/.local/bin (or $TILO_INSTALL_DIR).
# The archive for this machine must match its line in the release's
# SHA256SUMS, fetched over HTTPS from the same site, checked with sha256sum or
# shasum, which every Linux and macOS has. Where openssl is installed, the
# SHA256SUMS must also carry a signature by Tilo's release key (the key below,
# `keys/cosign.pub` in the Tilo repository: AWS KMS `alias/tilo-release-signing`,
# ECDSA P-256 over SHA-256); stock systems such as Fedora come without it, so
# its absence does not stop the install, only a signature that fails does.
# Anything that fails stops the install before a file is written.
#
# Where releases come from ($TILO_RELEASES, for a mirror; tools/ci/publish/cli-release.sh
# makes and publishes them with each release tag, and tilo.sh serves them):
#   $TILO_RELEASES/latest/SHA256SUMS        sha256sum lines of the archives
#   $TILO_RELEASES/latest/SHA256SUMS.sig    DER ECDSA signature of SHA256SUMS
#   $TILO_RELEASES/latest/tilo-<os>-<arch>.tar.gz   holding `tilo`
# $TILO_VERSION picks a release other than latest (`v0.4.0`).
#
# The whole script is one function called on its last line, so a download
# cut short runs nothing.
set -eu

main() {
  releases=${TILO_RELEASES:-https://tilo.sh/releases}
  version=${TILO_VERSION:-latest}
  dir=${TILO_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}

  say() { printf '%s\n' "$*" >&2; }
  fail() { say "tilo: $*"; exit 1; }
  need() { command -v "$1" >/dev/null 2>&1 || fail "this install needs $1, which this computer doesn't have"; }
  need uname
  need tar
  if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL --proto '=https,file' --tlsv1.2 -o "$2" "$1"; }
  elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
  else
    fail "this install needs curl or wget"
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    digest() { sha256sum "$1" | cut -d' ' -f1; }
  elif command -v shasum >/dev/null 2>&1; then
    digest() { shasum -a 256 "$1" | cut -d' ' -f1; }
  else
    fail "this install needs sha256sum or shasum"
  fi

  case $(uname -s) in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) fail "Tilo's command runs on Linux and macOS; this is $(uname -s)" ;;
  esac
  case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "Tilo's command runs on x86_64 and arm64; this is $(uname -m)" ;;
  esac
  archive=tilo-$os-$arch.tar.gz

  work=$(mktemp -d 2>/dev/null || mktemp -d -t tilo)
  trap 'rm -rf "$work"' EXIT INT TERM

  cat >"$work/release.pub" <<'KEY'
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEVwC4HzO//EjaTO+pMPheVjAaSD1y
9kSY4wHbsmT1bTYDT6ihc0T8K/hgGBpZtEqt/WwiJUIMBiyQu0/WnTdYUw==
-----END PUBLIC KEY-----
KEY
  if [ -n "${TILO_RELEASE_KEY:-}" ]; then
    # A mirror's own key (a test's): read from that file instead.
    cp "$TILO_RELEASE_KEY" "$work/release.pub"
  fi

  base=$releases/$version
  say "Getting Tilo ($version) for $os $arch…"
  fetch "$base/SHA256SUMS" "$work/SHA256SUMS" || fail "could not get $base/SHA256SUMS"
  if command -v openssl >/dev/null 2>&1; then
    fetch "$base/SHA256SUMS.sig" "$work/SHA256SUMS.sig" || fail "could not get the release's signature"
    openssl dgst -sha256 -verify "$work/release.pub" -signature "$work/SHA256SUMS.sig" "$work/SHA256SUMS" >/dev/null 2>&1 \
      || fail "the release's signature does not verify against Tilo's release key; nothing was installed"
  fi
  expected=$(awk -v name="$archive" '$2 == name || $2 == "*" name { print $1; exit }' "$work/SHA256SUMS")
  [ -n "$expected" ] || fail "this release has no $archive"
  fetch "$base/$archive" "$work/$archive" || fail "could not get $archive"
  [ "$(digest "$work/$archive")" = "$expected" ] || fail "$archive does not match the release's SHA256SUMS; nothing was installed"

  mkdir "$work/unpacked"
  tar -xzf "$work/$archive" -C "$work/unpacked" tilo || fail "$archive holds no tilo"
  [ -f "$work/unpacked/tilo" ] && [ ! -L "$work/unpacked/tilo" ] || fail "$archive holds no plain tilo"
  mkdir -p "$dir"
  # Replaced in one rename, so a tilo running now keeps working.
  cp "$work/unpacked/tilo" "$dir/.tilo.new"
  chmod 0755 "$dir/.tilo.new"
  mv -f "$dir/.tilo.new" "$dir/tilo"

  say "Installed tilo to $dir/tilo."
  case ":${PATH:-}:" in
    *":$dir:"*) ;;
    *) say "Add $dir to your PATH to run it as tilo, or run $dir/tilo." ;;
  esac
  say "Next: tilo login <code from your Tilo tab>, then tilo agent connect claude (or codex)."
}

main "$@"
