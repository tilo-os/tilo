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
#   $TILO_RELEASES/latest/tilo-<os>-<arch>.tar.gz   holding `tilo` and, for Linux,
#       `tilolang-process`, the program `tilo mcp` starts an agent's task programs in
# $TILO_VERSION picks a release other than latest (`v0.4.0`). A hosted setup
# requires OpenSSL and uses its own version directory on a fresh agent account.
#
# Arguments are a command for the installed `tilo`, run by its full path once
# it is in place: `curl -fsSL https://tilo.sh | sh -s login K7Q-MX2P` installs
# and logs in on a machine whose PATH does not name ~/.local/bin (a fresh one,
# or an agent's shell, which never rereads the profile that adds it).
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
    *) fail "Tilo's command runs on Linux and macOS; this is $(uname -s)" ;;
  esac
  case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "Tilo's command runs on x86_64 and arm64; this is $(uname -m)" ;;
  esac
  archive=tilo-$os-$arch.tar.gz

  if [ "${TILO_HOSTED_SETUP:-}" = 1 ]; then
    need openssl
    case "$version" in *[!A-Za-z0-9.-]*) fail "invalid hosted CLI prerelease" ;; esac
    if ! printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z.]+$'; then
      fail "hosted setup needs a named CLI prerelease"
    fi
    if [ "$dir" != "$HOME/.local/share/tilo/cli/$version" ] || [ -L "$dir" ]; then
      fail "hosted setup needs its version directory at $HOME/.local/share/tilo/cli/$version"
    fi
    # A login and its daemon belong to the account. A version-specific binary
    # alone cannot upgrade a different daemon, its registration, or its device.
    other_profile='This account already has a Tilo device or service. Use a fresh agent account or VM for this hosted CLI.'
    state=${XDG_STATE_HOME:-$HOME/.local/state}
    data=${XDG_DATA_HOME:-$HOME/.local/share}
    if [ "$os" = macos ]; then state="$HOME/Library/Application Support"; data=$state; fi
    keys=${TILO_IDENTITY_ROOT:-$data/tilo}
    if [ -z "${TILO_IDENTITY_ROOT:-}" ] && [ -d /var/lib/tilo/device ]; then keys=/var/lib/tilo; fi
    login=$state/tilo/device/login.json
    holder=$state/tilo/device/holder.lock
    receipt=$state/tilo/device/hosted-cli
    # The ticket's successful login writes its person's certificate and login record. Tie
    # the receipt to both public files, without reading the device's private signing key.
    hosted_identity() {
      [ -f "$login" ] || return 1
      person=$(sed -n 's/^[[:space:]]*"user":[[:space:]]*"\([0-9a-f]*\)".*$/\1/p' "$login")
      [ "${#person}" -eq 64 ] || return 1
      cert=$keys/device/certs/$person.bin
      [ -f "$cert" ] || return 1
      printf '%s %s %s' "$version" "$(digest "$login")" "$(digest "$cert")"
    }
    if [ -f "$receipt" ] && [ ! -L "$receipt" ]; then
      identity=$(hosted_identity) && [ "$identity" = "$(sed -n '1p' "$receipt")" ] || fail "$other_profile"
    else
      if [ -e "$receipt" ] || [ -L "$receipt" ] || [ -e "$login" ] || [ -e "$holder" ] ||
         [ -d "$keys/device" ] || [ -e "$keys/device-private/keypair.bin" ] ||
         [ -d "${XDG_CONFIG_HOME:-$HOME/.config}/tilo/identity" ] || [ "${1:-}" = device ]; then
        fail "$other_profile"
      fi
    fi
    if [ "$os" = linux ]; then
      service=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/tilo-peer.service
    else
      service=$HOME/Library/LaunchAgents/so.tilo.peer.plist
      if launchctl print "gui/$(id -u)/so.tilo.runtime" >/dev/null 2>&1; then fail "$other_profile"; fi
    fi
    if [ -e "$service" ] && ! grep -Fq "$dir/tilo-runtime" "$service"; then fail "$other_profile"; fi
  fi

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
  say "Getting Tilo ($version) for $os ${arch}…"
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
  # A Linux release also holds tilolang-process, which tilo looks for beside itself to start
  # an agent's task programs in. A Mac's archive holds none (no task program starts there), and
  # neither does a release from before it was packed: then tilo is installed alone.
  beside=
  tar -xzf "$work/$archive" -C "$work/unpacked" tilolang-process 2>/dev/null || :
  if [ -e "$work/unpacked/tilolang-process" ] || [ -L "$work/unpacked/tilolang-process" ]; then
    [ -f "$work/unpacked/tilolang-process" ] && [ ! -L "$work/unpacked/tilolang-process" ] \
      || fail "$archive holds no plain tilolang-process"
    beside=tilolang-process
  fi
  # The daemon `tilo login` and `tilo mcp` start (tilo-runtime), beside tilo, where they look
  # for it. A release from before it was packed holds none.
  tar -xzf "$work/$archive" -C "$work/unpacked" tilo-runtime 2>/dev/null || :
  if [ -e "$work/unpacked/tilo-runtime" ] || [ -L "$work/unpacked/tilo-runtime" ]; then
    [ -f "$work/unpacked/tilo-runtime" ] && [ ! -L "$work/unpacked/tilo-runtime" ] \
      || fail "$archive holds no plain tilo-runtime"
    beside="$beside tilo-runtime"
  fi
  if [ "${TILO_HOSTED_SETUP:-}" = 1 ]; then
    [ -f "$work/unpacked/tilo-runtime" ] || fail "the hosted release has no signed tilo-runtime"
    for name in $beside tilo; do
      if [ -L "$dir/$name" ] || { [ -e "$dir/$name" ] && ! cmp -s "$work/unpacked/$name" "$dir/$name"; }; then
        fail "$dir/$name differs from the signed CLI in this version directory"
      fi
    done
    # A daemon can also run on its own, with no user service. Its PID is kept in its
    # lock file. A free lock can have a stale PID; a live holder must run this binary.
    linux_generation() {
      pid=$(cat "$holder") || return 1
      case "$pid" in ''|*[!0-9]*) return 1 ;; esac
      boot=$(cat /proc/sys/kernel/random/boot_id) || return 1
      # Field 22 is the start time in kernel ticks. The command in parentheses may contain spaces.
      since=$(sed 's/^.*) //' "/proc/$pid/stat" | cut -d ' ' -f 20)
      case "$since" in ''|*[!0-9]*) return 1 ;; esac
      printf '%s %s %s' "$boot" "$pid" "$since"
    }
    hosted_service_holds() {
      [ -f "$service" ] && [ "$(systemctl --user show tilo-peer.service -p MainPID --value 2>/dev/null)" = "$pid" ] || return 1
      launch=$(systemctl --user show tilo-peer.service -p ExecStart --value 2>/dev/null) || return 1
      case "$launch" in "{ path=$dir/tilo-runtime ; "*) ;; *) return 1 ;; esac
      [ "$(tr '\000' '\n' < "/proc/$pid/cmdline" | sed -n '1p')" = "$dir/tilo-runtime" ]
    }
    generation=
    if [ -e "$holder" ]; then
      [ -f "$holder" ] && [ ! -L "$holder" ] || fail "$other_profile"
      if [ "$os" = linux ]; then
        need flock
        if ! flock -sn "$holder" true; then
          generation=$(linux_generation) || fail "$other_profile"
          # A daemon that read the private key is undumpable: even this user may no
          # longer read /proc/PID/exe. Its recorded kernel process identity survives that.
          if [ "$generation" != "$(sed -n '2p' "$receipt")" ]; then
            pid=$(cat "$holder")
            cmp -s "/proc/$pid/exe" "$work/unpacked/tilo-runtime" || hosted_service_holds || fail "$other_profile"
          fi
        fi
      else
        need lsof
        pid=$(lsof -n -t "$holder") || pid=
        if [ -n "$pid" ]; then
          case "$pid" in *[!0-9]*) fail "$other_profile" ;; esac
          lsof -n -a -p "$pid" -d txt -Fn | grep -Fxq "n$dir/tilo-runtime" || fail "$other_profile"
        fi
      fi
    fi
  fi
  mkdir -p "$dir"
  # Each replaced in one rename, so a tilo running now keeps working; the program beside it
  # first, so a new tilo never starts an old one.
  for name in $beside tilo; do
    if [ "${TILO_HOSTED_SETUP:-}" = 1 ] && [ -x "$dir/$name" ]; then continue; fi
    cp "$work/unpacked/$name" "$dir/.$name.new"
    chmod 0755 "$dir/.$name.new"
    mv -f "$dir/.$name.new" "$dir/$name"
  done

  say "Installed tilo to $dir/tilo."
  case ":${PATH:-}:" in
    *":$dir:"*) tilo=tilo ;;
    *) tilo=$dir/tilo; say "That folder isn't on your PATH yet, so this uses the full path." ;;
  esac
  if [ "$#" -eq 0 ]; then
    say "Next: $tilo login CODE, with CODE from Tilo (choose \"Set up with your agent\")."
    finished=1
    return
  fi
  rm -rf "$work"
  trap - EXIT INT TERM
  # Its input is the rest of this script's pipe, not the person's.
  "$dir/tilo" "$@" </dev/null
  if [ "${TILO_HOSTED_SETUP:-}" = 1 ]; then
    case "$1:${2:-}" in
      login:-*|login:) return 0 ;;
      login:*|device:add) ;;
      *) return 0 ;;
    esac
    identity=$(hosted_identity) || fail "the hosted login did not leave its device certificate"
    if [ "$os" = linux ] && [ -f "$holder" ]; then
      need flock
      if ! flock -sn "$holder" true; then
        running=$(linux_generation) || fail "$other_profile"
        if [ "$generation" != "$running" ]; then
          # This signed command started with no such holder. Confirm the path it used to
          # spawn its sibling before remembering this process across future invocations.
          pid=$(cat "$holder")
          exe=$(tr '\000' '\n' < "/proc/$pid/cmdline" | sed -n '1p')
          [ "$exe" = "$dir/tilo-runtime" ] || fail "$other_profile"
        fi
        generation=$running
      fi
    fi
    (umask 077; tmp=$(mktemp "$receipt.XXXXXXXX") && printf '%s\n%s\n' "$identity" "$generation" > "$tmp" && mv -f "$tmp" "$receipt")
  fi
}

main "$@"
