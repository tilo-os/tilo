#!/usr/bin/env bash
# The `tilo` command's release, as the installer (dist/install/tilo.sh, served at https://tilo.sh)
# installs it: one archive per platform, tilo-<os>-<arch>.tar.gz holding the program `tilo`,
# `tilo-runtime` beside it (this computer's one daemon, which `tilo login` and `tilo mcp` start
# from beside `tilo`) and, for Linux, `tilolang-process` (`tilo mcp` starts a task's tilolang
# programs in it and looks for it beside itself) and `tilo-launcher` (the daemon starts every
# app in its sandbox through it, and looks for it beside itself; on a Mac apps start through the
# launcher-macos library the daemon carries, so that archive holds no task program and no
# launcher), beside it tilolang-<os>-<arch>.tar.gz holding the `tilolang`
# command (the installer dist/install/tilolang.sh, served at https://tilolang.dev/install,
# installs it from the same releases), the SHA-256 of each in SHA256SUMS, and SHA256SUMS.sig, a DER ECDSA P-256
# signature of SHA256SUMS by the release key (keys/cosign.pub, AWS KMS
# alias/tilo-release-signing). It is published with the release tags (vX.Y.Z) that release the
# server and the OS, so a command talks to the server of its own release; `latest` follows the
# newest final version.
#
#   cli-release.sh build TARGET OUT    the program for TARGET, x86_64-unknown-linux-musl or
#                                      aarch64-unknown-linux-musl (statically linked, both on the
#                                      x86_64 Linux node: aarch64 cross-builds with the Zig
#                                      toolchain the check's setup installs), aarch64-apple-darwin
#                                      or x86_64-apple-darwin (on a Mac), packed as OUT/tilo-<os>-
#                                      <arch>.tar.gz; holds no credentials. Built for a tag, the
#                                      programs say that release as `tilo --version` and
#                                      `tilolang --version`.
#                                      `tilo-runtime` is built with it and packed beside it, and
#                                      for Linux `tilolang-process` and `tilo-launcher` too, each
#                                      as static as it is; `tilolang` is built in the same build
#                                      and packed as OUT/tilolang-<os>-<arch>.tar.gz
#   cli-release.sh pack PROGRAM ARCHIVE [TASK-PROGRAM [LAUNCHER]]
#                                      PROGRAM packed as the archive ARCHIVE, as build packs it,
#                                      with the `tilo-runtime` beside PROGRAM, TASK-PROGRAM beside
#                                      it as `tilolang-process`, and LAUNCHER beside it as
#                                      `tilo-launcher`
#   cli-release.sh pack-tilolang PROGRAM ARCHIVE
#                                      PROGRAM packed as the archive ARCHIVE, as `tilolang`
#   cli-release.sh sums OUT DIR...     the archives of each DIR together in OUT, with SHA256SUMS;
#                                      every Linux archive of both commands is required
#   cli-release.sh sign DIR            DIR/SHA256SUMS signed with the release key in KMS into
#                                      DIR/SHA256SUMS.sig, checked the way the installer checks
#                                      it; with TILO_SIGN_TLOG=1 also entered in the public
#                                      transparency log, its entry kept as SHA256SUMS.bundle.
#                                      Releases only (a vX.Y.Z tag).
#   cli-release.sh publish DIR         the signed release to code.tilo.so's package registry as
#                                      tilo-cli/<version>/<file>, and as tilo-cli/latest/<file>
#                                      for a final version newer than latest's; tilo.sh serves
#                                      both at /releases/ (the web entry's Caddyfile)
#   cli-release.sh rehearse DIR        sign (a throwaway key) and publish the release in DIR as
#                                      a tag would, serve it at a local site laid out as tilo.sh
#                                      serves it, and install it from there the way a person
#                                      does, `curl -fsSL https://<site>/ | sh`, then run what
#                                      was installed; nothing here reaches the live site
#
# With TILO_PUBLISH_DRY_RUN set to a directory, `sign` signs with a throwaway key made here
# (its public half kept as DIR/dry-run.pub) and `publish` writes DIR/releases/<version>/ and
# DIR/releases/latest/ there instead of uploading, for any reference; the version is the tag's,
# else v0.0.0. A signature with the key in KMS leaves DIR/dry-run.pub empty.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
usage='usage: cli-release.sh build TARGET OUT | pack PROGRAM ARCHIVE [TASK-PROGRAM [LAUNCHER]] | pack-tilolang PROGRAM ARCHIVE | sums OUT DIR... | sign DIR | publish DIR | rehearse DIR'
package=https://code.tilo.so/api/packages/tilo/generic/tilo-cli
# Each release carries both commands, `tilo` and `tilolang`, under one SHA256SUMS.
commands=(tilo tilolang)
linux_archives=() all_archives=()
for each in "${commands[@]}"; do
  linux_archives+=("$each-linux-x86_64.tar.gz" "$each-linux-aarch64.tar.gz")
  all_archives+=("$each-linux-x86_64.tar.gz" "$each-linux-aarch64.tar.gz"
    "$each-macos-aarch64.tar.gz" "$each-macos-x86_64.tar.gz")
done

# The version this run releases: the release tag's name, vX.Y.Z or vX.Y.Z-<pre-release>.
version() {
  if [[ ${T_REF:-} =~ ^tag:(v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?)$ ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
  elif [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    printf 'v0.0.0'
  else
    die "the tilo command is released from a release tag (vX.Y.Z); this run is for ${T_REF:-a working tree}"
  fi
}

# A final version (no pre-release part) as three numbers, for `sort -V`; nothing for any other.
final() { if [[ $1 =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then printf '%s' "${BASH_REMATCH[1]}"; fi; }

# Pack the archive $1 with the members NAME PATH that follow it, each mode 0755, owned by root
# and dated the epoch, gzip without a name or time, so the same programs always pack to the same
# bytes.
pack_archive() {
  python3 -I - "$@" <<'PY'
import gzip, io, sys, tarfile
archive = sys.argv[1]
members = list(zip(sys.argv[2::2], sys.argv[3::2]))
raw = io.BytesIO()
with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
    for name, path in members:
        data = open(path, "rb").read()
        info = tarfile.TarInfo(name)
        info.size, info.mode, info.mtime = len(data), 0o755, 0
        info.uid = info.gid = 0
        info.uname = info.gname = "root"
        tar.addfile(info, io.BytesIO(data))
with open(archive, "wb") as out, gzip.GzipFile(filename="", mode="wb", fileobj=out, mtime=0, compresslevel=9) as gz:
    gz.write(raw.getvalue())
PY
}

# Pack the program $1 as the archive $2, with the daemon beside $1, and the task program $3 and
# app launcher $4, each where it is given (a Linux archive has both): the members `tilo`,
# `tilo-runtime`, `tilolang-process` and `tilo-launcher`.
pack() {
  local runtime task=${3:-} launcher=${4:-}
  runtime=$(dirname -- "$1")/tilo-runtime
  [[ -f $runtime ]] || die "no $runtime beside $1: the daemon tilo starts is packed with it"
  [[ -z $task || -f $task ]] || die "no $task: the task program packed beside the command"
  [[ -z $launcher || -f $launcher ]] || die "no $launcher: the app launcher packed beside the command"
  pack_archive "$2" tilo "$1" tilo-runtime "$runtime" \
    ${task:+tilolang-process "$task"} ${launcher:+tilo-launcher "$launcher"}
}

# Pack the tilolang command $1 as the archive $2: its one member `tilolang`.
pack_tilolang() {
  pack_archive "$2" tilolang "$1"
}

# The otool that reads a Mach-O here: a Mac's own, or llvm's on the Linux node that builds one.
macho_otool() {
  if command -v otool >/dev/null; then
    printf 'otool'
    return
  fi
  command -v llvm-otool >/dev/null \
    || die 'no otool or llvm-otool to read a Mach-O with (the cli-macos-cross-ARCH check installs llvm)'
  printf 'llvm-otool'
}

# The libraries a Mach-O names, one path per line, as dyld looks them up.
macho_dylibs() {
  "$(macho_otool)" -L "$1" | tail -n +2 | awk '{print $1}'
}

# What is checked of a program that cannot run here, read out of the Mach-O it is: the
# architecture it was built for, the oldest macOS it runs on, that every library it names is one
# every Mac carries -- a framework's stub names it the way the framework is installed -- and that
# an arm64 program still carries the ad-hoc signature its build signs it with, without which an
# arm64 Mac refuses to run it.
verify_macho() {
  local program=$1 arch=$2 otool load_commands cpu minos foreign
  otool=$(macho_otool)
  cpu=$("$otool" -hv "$program" | awk '$1 == "MH_MAGIC_64" { print $2 }')
  case $arch in
    aarch64) [[ $cpu == ARM64 ]] || die "$program is a Mach-O of $cpu, not of the ARM64 $arch needs" ;;
    x86_64) [[ $cpu == X86_64 ]] || die "$program is a Mach-O of $cpu, not of the X86_64 $arch needs" ;;
  esac
  # Read once, and out of a file: a reader that stops at its first match closes the pipe under
  # otool, and this script's `set -e` fails on that SIGPIPE.
  load_commands=$("$otool" -l "$program")
  minos=$(awk '$1 == "minos" && !found++ { print $2 }' <<< "$load_commands")
  [[ $minos == 11.0 ]] \
    || die "$program was built for macOS ${minos:-no version at all}, not for the 11.0 the build asks for"
  foreign=$(macho_dylibs "$program" | grep -Ev '^(/usr/lib/|/System/Library/)' || true)
  [[ -z $foreign ]] || die "$program names libraries a Mac does not carry: $foreign"
  if [[ $arch == aarch64 ]]; then
    grep 'cmd LC_CODE_SIGNATURE' <<< "$load_commands" >/dev/null \
      || die "$program carries no code signature, which an arm64 Mac needs to run it"
  fi
}

# The task program started with no api handed to it (the words are the program, behind an
# emulator where it is another architecture's) says so in its own words and ends, which one
# that cannot start here would not.
task_program_starts() {
  local said
  if said=$("$@" 2>&1); then die "$* starts with no api handed to it"; fi
  grep -q 'tilolang-process: exiting: no api' <<< "$said" || die "$* said: $said"
}

cmd_build() {
  local target=${1:?$usage} out=${2:?$usage} os arch flags cross stubs
  case $target in
    x86_64-unknown-linux-musl) os=linux arch=x86_64 ;;
    aarch64-unknown-linux-musl) os=linux arch=aarch64 ;;
    aarch64-apple-darwin) os=macos arch=aarch64 ;;
    x86_64-apple-darwin) os=macos arch=x86_64 ;;
    *) die "no release of the tilo command is built for $target" ;;
  esac
  # The program runs on any machine of its architecture, not only on CI's: RUSTFLAGS replaces
  # .cargo/config.toml's flags, whose x86_64 floor (x86-64-v3) is the OS image's, not every
  # computer's. Paths of this checkout and of Cargo's sources are not the program's business.
  flags="-C force-frame-pointers=yes --remap-path-prefix=$repo_root=. --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo"
  if [[ $os == linux ]]; then
    [[ $(uname -s) == Linux ]] || die "$target is built on a Linux node, not on $(uname -s)"
    # Both builds link a musl newer than 1.2.3 -- the distribution's, and Zig's for the other
    # architecture -- and libc's build script takes that from this variable: without it libc
    # declares no `statx` for musl, and the code that reads a file's mount id does not compile.
    export CARGO_CFG_LIBC_UNSTABLE_MUSL_V1_2_3=1
    if [[ $(uname -m) == "$arch" ]]; then
      # musl's C compiler for the C the program links (ring, SQLite), which the check's setup
      # installs: the C must be compiled against musl's headers, not the system's.
      command -v musl-gcc >/dev/null || die 'no musl-gcc: install it (the cli-ARCH check sets it up)'
      export "CC_${target//-/_}=musl-gcc"
    else
      # The other architecture, from this node: Zig's compiler carries musl's headers and libc
      # for it, and the check's setup installs it. It compiles the same C against that musl and
      # links the program with it, rustc's own bundled musl left out, so the program is one musl
      # and nothing of this node's glibc reaches the archive.
      command -v zig >/dev/null \
        || die "no zig, which cross-builds $arch: install it (the cli-$arch check sets it up)"
      cross=$repo_root/tools/ci/zig-cc.sh
      export TILO_ZIG_TARGET=$arch-linux-musl
      export "CC_${target//-/_}=$cross"
      # Zig's caches belong to this build, not to the node: a later one starts from none of it.
      export ZIG_GLOBAL_CACHE_DIR=$scratch/zig-cache ZIG_LOCAL_CACHE_DIR=$scratch/zig-cache
      flags="$flags -C linker=$cross -C link-self-contained=no"
    fi
    flags="$flags -C target-feature=+crt-static -C relro-level=full -C link-arg=-Wl,-z,noexecstack"
  else
    # The oldest macOS the program runs on, for Rust and the C cc-rs compiles alike.
    export MACOSX_DEPLOYMENT_TARGET=11.0
    case $(uname -s) in
      Darwin) rustup target add "$target" >/dev/null ;;
      Linux)
        # No Mac is a build node: Zig builds the program here, for either Mac architecture. It
        # carries Darwin's C library headers, which is what the C the program links (ring, SQLite,
        # zstd) is compiled against, and the framework stubs the check's setup writes stand in for
        # the frameworks' headers and libraries (tools/ci/macos-frameworks.py). No Apple SDK is
        # involved: it is Apple's to license on Apple hardware, and nothing here is a copy of it.
        command -v zig >/dev/null \
          || die "no zig, which cross-builds $target: install it (the cli-macos-cross-$arch check sets it up)"
        stubs=$scratch/macos-frameworks
        rm -rf "$stubs"
        python3 -I "$repo_root/tools/ci/macos-frameworks.py" "$stubs" "$target"
        cross=$repo_root/tools/ci/zig-cc.sh
        export TILO_ZIG_TARGET=$arch-macos.11.0
        export "CC_${target//-/_}=$cross"
        # Zig's caches belong to this build, not to the node: a later one starts from none of it.
        export ZIG_GLOBAL_CACHE_DIR=$scratch/zig-cache ZIG_LOCAL_CACHE_DIR=$scratch/zig-cache
        flags="$flags -C linker=$cross -C link-arg=-F$stubs -C link-arg=-L$stubs"
        ;;
      *) die "$target is built on Linux or a Mac, not on $(uname -s)" ;;
    esac
  fi
  # The release's version, so `tilo --version` and `tilolang --version` name the release these
  # programs are from. A build that releases nothing (a branch's, a working tree's) keeps the
  # crates' own version.
  local released
  if [[ ${T_REF:-} == tag:* || -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    released=$(version)
    export TILO_CLI_VERSION=${released#v}
  fi
  # `tilo mcp` starts a task's tilolang programs in tilolang-process, which it looks for beside
  # itself, on Linux alone: there the archive carries it, built with the command in one feature
  # resolution. On a Mac no task program starts, and none is built.
  # `tilo login` and `tilo mcp` start this computer's one daemon, tilo-runtime, from beside
  # `tilo`: every archive carries it.
  # That daemon starts every app in its sandbox through tilo-launcher, which it looks for beside
  # itself too. The launcher is Linux's, so it is built and carried there alone: on a Mac the
  # daemon confines apps itself, through the launcher-macos library in it.
  # The tilolang command is built in the same build and released beside tilo, under the same
  # sums and signature.
  local packages=(-p tilo-cli --bin tilo -p tilo-daemon --bin tilo-runtime -p tilolang-cli --bin tilolang)
  local task_program='' launcher='' built tilolang
  if [[ $os == linux ]]; then
    packages+=(-p tilolang-process --bin tilolang-process -p launcher --bin tilo-launcher)
  fi
  RUSTFLAGS=$flags CARGO_PROFILE_RELEASE_STRIP=symbols CARGO_INCREMENTAL=0 \
    cargo build --locked --release --target "$target" "${packages[@]}"
  local program=${CARGO_TARGET_DIR:-$repo_root/target}/$target/release/tilo
  [[ -f $program ]] || die "the build left no $program"
  tilolang=${program%/tilo}/tilolang
  [[ -f $tilolang ]] || die "the build left no $tilolang"
  if [[ $os == linux ]]; then
    task_program=${program%/tilo}/tilolang-process
    [[ -f $task_program ]] || die "the build left no $task_program"
    launcher=${program%/tilo}/tilo-launcher
    [[ -f $launcher ]] || die "the build left no $launcher"
    # Static: no program interpreter, no shared library, so each runs on any Linux distribution.
    for built in "$program" "${program%/tilo}/tilo-runtime" "$task_program" "$launcher" "$tilolang"; do
      if LC_ALL=C readelf -lW "$built" | grep -q 'Requesting program interpreter'; then
        die "$built names a program interpreter: it is not statically linked"
      fi
      if LC_ALL=C readelf -dW "$built" | grep -q '(NEEDED)'; then
        die "$built needs shared libraries: $(readelf -dW "$built" | grep '(NEEDED)')"
      fi
    done
    # The program runs here when it is this machine's architecture, and under qemu's user-mode
    # emulator when it is the other one, which the cli-aarch64 check's setup installs. Without the
    # emulator a cross-built archive is checked only by its ELF, above.
    if [[ $arch == $(uname -m) ]]; then
      "$program" --help >/dev/null || die "$program --help failed"
      if [[ -n ${TILO_CLI_VERSION:-} ]]; then
        [[ $("$program" --version) == "tilo $TILO_CLI_VERSION" ]] \
          || die "$program --version does not name the release $TILO_CLI_VERSION it was built for"
        [[ $("$tilolang" --version) == "tilolang $TILO_CLI_VERSION" ]] \
          || die "$tilolang --version does not name the release $TILO_CLI_VERSION it was built for"
      fi
      task_program_starts "$task_program"
      "$launcher" --help >/dev/null || die "$launcher --help failed"
      "$tilolang" --help >/dev/null || die "$tilolang --help failed"
    elif command -v "qemu-$arch-static" >/dev/null; then
      "qemu-$arch-static" "$program" --help >/dev/null \
        || die "$program --help failed under qemu-$arch-static"
      task_program_starts "qemu-$arch-static" "$task_program"
      "qemu-$arch-static" "$launcher" --help >/dev/null \
        || die "$launcher --help failed under qemu-$arch-static"
      "qemu-$arch-static" "$tilolang" --help >/dev/null \
        || die "$tilolang --help failed under qemu-$arch-static"
    fi
  else
    # A Mac's program does not run on a node of this fleet: what is checked of it is the file it
    # is. A Mac checks it by running it too, when the program is the architecture the Mac is.
    verify_macho "$program" "$arch"
    verify_macho "${program%/tilo}/tilo-runtime" "$arch"
    verify_macho "$tilolang" "$arch"
    if [[ $(uname -s) == Darwin && $arch == $(uname -m) ]]; then
      "$program" --help >/dev/null || die "$program --help failed"
      "$tilolang" --help >/dev/null || die "$tilolang --help failed"
    fi
  fi
  rm -rf "$out"
  mkdir -p "$out"
  pack "$program" "$out/tilo-$os-$arch.tar.gz" ${task_program:+"$task_program"} ${launcher:+"$launcher"}
  pack_tilolang "$tilolang" "$out/tilolang-$os-$arch.tar.gz"
  if command -v sha256sum >/dev/null; then
    (cd "$out" && sha256sum "tilo-$os-$arch.tar.gz" "tilolang-$os-$arch.tar.gz")
  else
    (cd "$out" && shasum -a 256 "tilo-$os-$arch.tar.gz" "tilolang-$os-$arch.tar.gz")
  fi
}

cmd_sums() {
  local out=${1:?$usage} dir name
  shift
  (($#)) || die "$usage"
  rm -rf "$out"
  mkdir -p "$out"
  for dir; do
    for name in "${all_archives[@]}"; do
      [[ -f $dir/$name ]] || continue
      [[ ! -e $out/$name ]] || die "two builds of $name: in $dir and before it"
      cp "$dir/$name" "$out/"
    done
  done
  for name in "${linux_archives[@]}"; do
    [[ -f $out/$name ]] || die "no $name among $*: a release carries every Linux archive of both commands"
  done
  (cd "$out" && LC_ALL=C sha256sum -- *.tar.gz | LC_ALL=C sort -k2 > SHA256SUMS)
  cat "$out/SHA256SUMS"
}

# The installer's check, word for word (dist/install/tilo.sh): DIR's signature against KEY.
installer_verifies() {
  command -v openssl >/dev/null || die 'no openssl, which checks a signature as the installer does'
  openssl dgst -sha256 -verify "$2" -signature "$1/SHA256SUMS.sig" "$1/SHA256SUMS" >/dev/null
}

cmd_sign() {
  local dir=${1:?$usage} public_key=$repo_root/keys/cosign.pub signature
  [[ -s $dir/SHA256SUMS ]] || die "no SHA256SUMS in $dir"
  version >/dev/null
  if [[ -z ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    [[ $(channel) == release ]] || die "only releases are signed; this run is for $T_REF"
  fi
  rm -f "$dir/SHA256SUMS.sig" "$dir/SHA256SUMS.bundle" "$dir/dry-run.pub"
  use_cosign
  # Cosign writes the signature base64-encoded; the installer reads it as DER, as openssl does.
  signature=$scratch/cli-signature.b64
  if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    local keys=$scratch/cli-dry-run-signing
    rm -rf "$keys"
    mkdir -p "$keys"
    (cd "$keys" && COSIGN_PASSWORD= cosign generate-key-pair >/dev/null)
    COSIGN_PASSWORD= cosign sign-blob --yes --tlog-upload=false --key "$keys/cosign.key" \
      --output-signature "$signature" "$dir/SHA256SUMS"
    cp "$keys/cosign.pub" "$dir/dry-run.pub"
    public_key=$dir/dry-run.pub
  else
    : > "$dir/dry-run.pub"
    . "$repo_root/os/dev/configure-cosign-kms.sh"
    if [[ ${TILO_SIGN_TLOG:-} == 1 ]]; then
      cosign sign-blob --yes --key "$COSIGN_KEY_REF" --bundle "$dir/SHA256SUMS.bundle" \
        --output-signature "$signature" "$dir/SHA256SUMS"
    else
      cosign sign-blob --yes --tlog-upload=false --key "$COSIGN_KEY_REF" \
        --output-signature "$signature" "$dir/SHA256SUMS"
    fi
  fi
  base64 -d < "$signature" > "$dir/SHA256SUMS.sig"
  installer_verifies "$dir" "$public_key" \
    || die "the signature of SHA256SUMS does not verify against $public_key as the installer checks it"
  printf 'signed SHA256SUMS of tilo and tilolang %s\n' "$(version)"
}

# Upload the files of DIR named by the remaining arguments to the package version URL $1.
upload() {
  local base=$1 dir=$2 name
  shift 2
  for name; do printf '%s\t%s\n' "$dir/$name" "$base/$name"; done > "$scratch/cli-uploads.tsv"
  TILO_PACKAGE_TOKEN=$TILO_PACKAGES_TOKEN TILO_PACKAGE_UPLOAD_WORKERS=4 \
    bash "$repo_root/tools/upload_immutable_packages.sh" "$scratch/cli-uploads.tsv"
}

cmd_publish() {
  local dir=${1:?$usage} v name files=() current newest
  v=$(version)
  [[ -s $dir/SHA256SUMS && -s $dir/SHA256SUMS.sig ]] || die "$dir holds no signed SHA256SUMS"
  local key=$repo_root/keys/cosign.pub
  if [[ -n ${TILO_PUBLISH_DRY_RUN:-} && -s $dir/dry-run.pub ]]; then key=$dir/dry-run.pub; fi
  installer_verifies "$dir" "$key" || die "$dir's signature does not verify against $key"
  (cd "$dir" && sha256sum --check --strict --quiet SHA256SUMS) || die "$dir's archives are not the ones SHA256SUMS names"
  printf '%s\n' "$v" > "$scratch/version"
  cp "$scratch/version" "$dir/version"
  # The archives first and the sums last: a reader that finds SHA256SUMS finds what it names.
  for name in "${all_archives[@]}"; do
    if [[ -f $dir/$name ]]; then files+=("$name"); fi
  done
  if [[ -s $dir/SHA256SUMS.bundle ]]; then files+=(SHA256SUMS.bundle); fi
  files+=(version SHA256SUMS.sig SHA256SUMS)
  if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
    local root=$TILO_PUBLISH_DRY_RUN/releases
    for name in "$v" latest; do
      if [[ $name == latest && -z $(final "$v") ]]; then continue; fi
      rm -rf "${root:?}/$name"
      mkdir -p "$root/$name"
      for f in "${files[@]}"; do cp "$dir/$f" "$root/$name/"; done
    done
    printf 'published tilo and tilolang %s into %s\n' "$v" "$root"
    return
  fi
  [[ $(channel) == release ]] || die "only releases are published; this run is for $T_REF"
  [[ -n ${TILO_PACKAGES_TOKEN:-} ]] || die 'TILO_PACKAGES_TOKEN is required: the check needs secrets = ["TILO_PACKAGES_TOKEN"]'
  # Each version once: a file already there must be this one (upload_immutable_packages.sh).
  upload "$package/$v" "$dir" "${files[@]}"
  echo "$package/$v/"
  [[ -n $(final "$v") ]] || { echo "$v is a pre-release: latest stays"; return; }
  current=$(curl -fsS --retry 3 "$package/latest/version" 2>/dev/null) || current=
  if [[ -n $current ]]; then
    [[ -n $(final "$current") ]] || die "latest names $current, which is not a final version"
    newest=$(printf '%s\n%s\n' "$(final "$current")" "$(final "$v")" | sort -V | tail -n 1)
    if [[ $newest != "$(final "$v")" || $current == "$v" ]]; then
      echo "latest is $current; leaving it"
      return
    fi
  fi
  # Package files never change in place: latest is replaced whole. While it is, an install
  # fails (no SHA256SUMS, or archives the sums do not name) and installs nothing.
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE \
    -H "Authorization: token $TILO_PACKAGES_TOKEN" "$package/latest")
  [[ $code == 204 || $code == 404 ]] || die "could not replace $package/latest (HTTP $code)"
  upload "$package/latest" "$dir" "${files[@]}"
  echo "$package/latest/ is $v"
}

# The files a site serves: the installer at / (what `curl -fsSL https://tilo.sh | sh` gets) and at
# /install.sh, beside the release ROOT already holds at /releases/<version>/ and
# /releases/latest/, as the web entry's Caddyfile maps them onto the package registry; and
# tilolang's installer at /tilolang.sh, which tilolang.dev serves at /install.
site_root() {
  local root=$1
  cp "$repo_root/dist/install/tilo.sh" "$root/index.html"
  cp "$repo_root/dist/install/tilo.sh" "$root/install.sh"
  cp "$repo_root/dist/install/tilolang.sh" "$root/tilolang.sh"
}

# Serve ROOT with CERT and KEY as the site's at https://127.0.0.1:$port, the port the kernel picks:
# the installer fetches over https alone, so a rehearsal's site is https too, and curl is told to
# trust this certificate. The site writes the port it bound to $5 as soon as it listens.
serve() {
  local root=$1 cert=$2 key=$3 log=$4 portfile=$5
  python3 - "$root" "$cert" "$key" "$portfile" >"$log" 2>&1 <<'PY' &
import functools, http.server, ssl, sys
root, cert, key, portfile = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
site = http.server.HTTPServer(("127.0.0.1", 0), functools.partial(http.server.SimpleHTTPRequestHandler, directory=root))
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(cert, key)
site.socket = context.wrap_socket(site.socket, server_side=True)
with open(portfile, "w") as written:
  written.write(f"{site.server_address[1]}\n")
site.serve_forever()
PY
}

# The port the site serving beside $1 wrote there, once it listens.
site_port() {
  local portfile=$1 waited=0
  while (( waited < 300 )); do
    if [[ -s $portfile ]]; then
      cat "$portfile"
      return 0
    fi
    sleep 0.1
    (( waited += 1 ))
  done
  return 1
}

# Install the release the site at $1 serves, as a person does: the installer is what `curl` gets
# from that site's root, piped into `sh`, and everything it runs with is the environment `$2`
# gives. The site's certificate is trusted through CURL_CA_BUNDLE, which the installer's own
# `curl` reads; $3 picks a release other than latest, and $6 the installer's path on the site
# (tilo's at / by default).
install_from_site() {
  local site=$1 path=$2 ref=$3 home=$4 dir=$5 installer=${6:-}
  curl -fsSL --cacert "$dir/site.pem" "$site/$installer" \
    | env -i PATH="$path" HOME="$home" CURL_CA_BUNDLE="$dir/site.pem" \
        TILO_RELEASES="$site/releases" TILO_VERSION="$ref" TILO_RELEASE_KEY="$dir/dry-run.pub" sh
}

cmd_rehearse() {
  local release=${1:?$usage} installed ref machine command
  # Not `local`: the EXIT trap below runs after this function has returned.
  server=
  site=$scratch/cli-rehearsal-site
  dir=$scratch/cli-rehearsal-release
  rm -rf "$site" "$dir"
  mkdir -p "$site"
  cp -r "$release" "$dir"
  export TILO_PUBLISH_DRY_RUN=$site
  cmd_sign "$dir"
  cmd_publish "$dir"
  site_root "$site"
  # The site's certificate, made here: the name it answers to is this machine's loopback.
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 \
    -subj /CN=cli-rehearsal -addext subjectAltName=IP:127.0.0.1 \
    -keyout "$dir/site.key" -out "$dir/site.pem" >/dev/null 2>&1 \
    || die 'could not make the rehearsal site a certificate'
  site_port_file=$scratch/cli-rehearsal-site.port
  rm -f "$site_port_file"
  serve "$site" "$dir/site.pem" "$dir/site.key" "$scratch/cli-rehearsal-site.log" "$site_port_file"
  server=$!
  trail() {
    kill "$server" 2>/dev/null || true
    rm -rf "$site" "$dir" "$scratch/cli-rehearsal-home" "$scratch/cli-rehearsal-tools" \
      "$scratch/cli-rehearsal-site.log" "$site_port_file"
  }
  trap trail EXIT
  port=$(site_port "$site_port_file") \
    || die "the rehearsal's site did not come up: $(cat "$scratch/cli-rehearsal-site.log" 2>/dev/null)"
  local url=https://127.0.0.1:$port
  curl -fsS --retry 30 --retry-connrefused --retry-delay 1 --cacert "$dir/site.pem" -o /dev/null "$url/install.sh" \
    || die "the rehearsal's site did not come up: $(cat "$scratch/cli-rehearsal-site.log" 2>/dev/null)"
  case $(uname -m) in x86_64) machine=x86_64 ;; aarch64) machine=aarch64 ;; *) die "no release for $(uname -m)" ;; esac
  local refs=("$(version)")
  if [[ -n $(final "$(version)") ]]; then refs+=(latest); fi
  home=$scratch/cli-rehearsal-home
  for ref in "${refs[@]}"; do
    rm -rf "$home"
    mkdir -p "$home"
    # The installer as tilo.sh serves it, from the site's own layout, with the throwaway key as
    # the mirror's: it checks the signature and the archive's sum, then installs.
    install_from_site "$url" /usr/bin:/bin "$ref" "$home" "$dir" \
      || die "the installer refused the rehearsed release at $ref"
    installed=$home/.local/bin/tilo
    cmp --silent "$installed" <(tar -xzOf "$site/releases/$ref/tilo-linux-$machine.tar.gz" tilo) \
      || die "the installer installed something other than tilo-linux-$machine.tar.gz's program"
    HOME=$home "$installed" --help >/dev/null || die "the installed tilo does not run"
    # The program `tilo mcp` starts a task's programs in: beside the command, where it looks.
    cmp --silent "$home/.local/bin/tilolang-process" \
      <(tar -xzOf "$site/releases/$ref/tilo-linux-$machine.tar.gz" tilolang-process) \
      || die "the installer did not install tilo-linux-$machine.tar.gz's task program beside tilo"
    task_program_starts "$home/.local/bin/tilolang-process"
    # The daemon `tilo login` starts, and the launcher it starts every app through: beside the
    # command, where each looks.
    cmp --silent "$home/.local/bin/tilo-runtime" \
      <(tar -xzOf "$site/releases/$ref/tilo-linux-$machine.tar.gz" tilo-runtime) \
      || die "the installer did not install tilo-linux-$machine.tar.gz's daemon beside tilo"
    cmp --silent "$home/.local/bin/tilo-launcher" \
      <(tar -xzOf "$site/releases/$ref/tilo-linux-$machine.tar.gz" tilo-launcher) \
      || die "the installer did not install tilo-linux-$machine.tar.gz's app launcher beside tilo"
    "$home/.local/bin/tilo-launcher" --help >/dev/null || die "the installed tilo-launcher does not run"
    # tilolang's installer, from the same release.
    rm -rf "$home"
    mkdir -p "$home"
    install_from_site "$url" /usr/bin:/bin "$ref" "$home" "$dir" tilolang.sh \
      || die "tilolang's installer refused the rehearsed release at $ref"
    cmp --silent "$home/.local/bin/tilolang" \
      <(tar -xzOf "$site/releases/$ref/tilolang-linux-$machine.tar.gz" tilolang) \
      || die "tilolang's installer installed something other than tilolang-linux-$machine.tar.gz's program"
    HOME=$home "$home/.local/bin/tilolang" --help >/dev/null || die "the installed tilolang does not run"
  done
  # Stock systems (Fedora) come without openssl, so the installer must install there too, on the
  # archive's sum against SHA256SUMS alone: only what the installer itself runs is on PATH.
  tools=$scratch/cli-rehearsal-tools
  rm -rf "$tools"
  mkdir -p "$tools"
  for command in sh uname tar gzip curl sha256sum mktemp awk mkdir cp chmod mv rm cut cat; do
    ln -s "$(command -v "$command")" "$tools/$command" || die "no $command to give the installer a PATH without openssl"
  done
  PATH=$tools command -v openssl >/dev/null && die 'openssl is on the PATH the rehearsal calls empty of it'
  rm -rf "$home"
  mkdir -p "$home"
  install_from_site "$url" "$tools" latest "$home" "$dir" \
    || die 'without openssl the installer refused the release'
  cmp --silent "$home/.local/bin/tilo" <(tar -xzOf "$site/releases/latest/tilo-linux-$machine.tar.gz" tilo) \
    || die 'without openssl the installer installed something other than the release'
  cmp --silent "$home/.local/bin/tilolang-process" \
    <(tar -xzOf "$site/releases/latest/tilo-linux-$machine.tar.gz" tilolang-process) \
    || die 'without openssl the installer did not install the task program beside tilo'
  cmp --silent "$home/.local/bin/tilo-launcher" \
    <(tar -xzOf "$site/releases/latest/tilo-linux-$machine.tar.gz" tilo-launcher) \
    || die 'without openssl the installer did not install the app launcher beside tilo'
  # The site's files decide: one that does not match its sums, and a release the release key did
  # not sign, both install nothing.
  printf 'x' >> "$site/releases/latest/tilo-linux-$machine.tar.gz"
  rm -rf "$home"
  mkdir -p "$home"
  if install_from_site "$url" /usr/bin:/bin latest "$home" "$dir" 2>/dev/null; then
    die 'the installer accepted an archive that does not match the release SHA256SUMS'
  fi
  [[ ! -e $home/.local/bin/tilo && ! -e $home/.local/bin/tilolang-process ]] \
    || die 'the installer wrote a program it refused'
  rm -f "$site/releases/latest/SHA256SUMS.sig"
  if install_from_site "$url" /usr/bin:/bin latest "$home" "$dir" 2>/dev/null; then
    die 'the installer accepted a release the release key did not sign'
  fi
  [[ ! -e $home/.local/bin/tilo && ! -e $home/.local/bin/tilolang-process ]] \
    || die 'the installer wrote a program it refused'
  echo "the installers install the rehearsed release $(version) of tilo and tilolang from the rehearsal's own site, tilo's with and without openssl, and refuse a release its sums or the release key do not match"
}

case ${1:-} in
  build) shift; cmd_build "$@" ;;
  # $3 and $4 are passed even when empty: a caller that has the launcher but no task program
  # names it in its own place rather than having it read as the task program.
  pack) shift; pack "${1:?$usage}" "${2:?$usage}" "${3-}" "${4-}" ;;
  pack-tilolang) shift; pack_tilolang "${1:?$usage}" "${2:?$usage}" ;;
  sums) shift; cmd_sums "$@" ;;
  sign) shift; cmd_sign "$@" ;;
  publish) shift; cmd_publish "$@" ;;
  rehearse) shift; cmd_rehearse "$@" ;;
  *) die "$usage" ;;
esac
