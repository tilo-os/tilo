#!/bin/sh
# Install the `tilolang` command, as `curl -fsSL https://tilolang.dev/install | sh` runs it.
#
# This script is dist/install/tilo.sh with this comment and the values between its two
# `# installs:` lines replaced (dist/install/installers.py writes it; edit tilo.sh). Everything
# tilo.sh says about how it fetches, verifies and installs holds here: the command goes to
# ~/.local/bin (or $TILO_INSTALL_DIR) without root; the archive for this machine,
# tilolang-<os>-<arch>.tar.gz, must match its line in the release's SHA256SUMS; where openssl is
# installed, SHA256SUMS must carry a signature by Tilo's release key; anything that fails stops
# the install before a file is written.
#
# `tilolang` is released with `tilo`, from the same releases ($TILO_RELEASES, default
# https://tilo.sh/releases; $TILO_VERSION picks a release other than latest), under the same
# SHA256SUMS and signature. Arguments are a command for the installed `tilolang`, run by its full
# path once it is in place: `curl -fsSL https://tilolang.dev/install | sh -s check .`.
#
# The whole script is one function called on its last line, so a download
# cut short runs nothing.
set -eu

main() {
  # installs: what this script installs, and what it says once it has.
  program=tilolang
  title=tilolang
  # A tilolang release packs the command alone.
  beside_programs=
  say_next() { say "Next: $1 check DIR checks the package in DIR; $1 build DIR builds it into an app."; }
  # installs: end
  releases=${TILO_RELEASES:-https://tilo.sh/releases}
  version=${TILO_VERSION:-latest}
  dir=${TILO_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}

  say() { printf '%s\n' "$*" >&2; }
  fail() { say "$program: $*"; exit 1; }
  need() { command -v "$1" >/dev/null 2>&1 || fail "this install needs $1, which this computer doesn't have"; }
  need uname
  need tar
  # Releases come over https, or over plain http from this computer itself
  # (a copy of tilo.sh running here), which never leaves it.
  protocols='=https,file'
  case $releases in
    http://*)
      host=${releases#http://}
      host=${host%%/*}
      case $host in \[*) host=${host%%]*}] ;; *) host=${host%:*} ;; esac
      case $host in
        localhost | *.localhost | '[::1]') protocols='=https,http,file' ;;
        127.*) case $host in *[!0-9.]*) ;; *) protocols='=https,http,file' ;; esac ;;
      esac ;;
  esac
  if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL --proto "$protocols" --tlsv1.2 -o "$2" "$1"; }
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
    *) fail "$title runs on Linux and macOS; this is $(uname -s)" ;;
  esac
  case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "$title runs on x86_64 and arm64; this is $(uname -m)" ;;
  esac
  archive=$program-$os-$arch.tar.gz

  work=$(mktemp -d 2>/dev/null || mktemp -d -t tilo)
  # An install that stops early must not end as a success. macOS's sh (bash 3.2) ends a script
  # that stopped on an unset variable with status 0 once an exit trap is set, so the trap asks
  # whether the install said it finished, and fails if it did not.
  finished=
  trap 'status=$?; rm -rf "$work"; [ -n "$finished" ] || [ "$status" -ne 0 ] || status=1; exit "$status"' EXIT
  trap 'rm -rf "$work"; trap - EXIT; exit 130' INT
  trap 'rm -rf "$work"; trap - EXIT; exit 143' TERM

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
  # ${arch}, in braces: in a UTF-8 locale macOS's sh takes the first byte of the character
  # after a bare name as part of the name, and stops on a variable that is not set.
  say "Getting $title ($version) for $os ${arch}…"
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
  tar -xzf "$work/$archive" -C "$work/unpacked" "$program" || fail "$archive holds no $program"
  [ -f "$work/unpacked/$program" ] && [ ! -L "$work/unpacked/$program" ] || fail "$archive holds no plain $program"
  beside=
  for name in $beside_programs; do
    tar -xzf "$work/$archive" -C "$work/unpacked" "$name" 2>/dev/null || :
    if [ -e "$work/unpacked/$name" ] || [ -L "$work/unpacked/$name" ]; then
      [ -f "$work/unpacked/$name" ] && [ ! -L "$work/unpacked/$name" ] \
        || fail "$archive holds no plain $name"
      beside="$beside $name"
    fi
  done
  mkdir -p "$dir"
  # Each replaced in one rename, so a program running now keeps working; the programs beside it
  # first, so a new one never starts an old one.
  for name in $beside "$program"; do
    cp "$work/unpacked/$name" "$dir/.$name.new"
    chmod 0755 "$dir/.$name.new"
    mv -f "$dir/.$name.new" "$dir/$name"
  done

  say "Installed $program to $dir/$program."
  case ":${PATH:-}:" in
    *":$dir:"*) run=$program ;;
    *) run=$dir/$program; say "That folder isn't on your PATH yet, so this uses the full path." ;;
  esac
  if [ "$#" -eq 0 ]; then
    say_next "$run"
    finished=1
    return
  fi
  rm -rf "$work"
  trap - EXIT INT TERM
  # Its input is the rest of this script's pipe, not the person's.
  "$dir/$program" "$@" </dev/null
}

main "$@"
