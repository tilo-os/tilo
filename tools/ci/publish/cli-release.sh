#!/usr/bin/env bash
# The `tilo` command's release, as the installer (dist/install/tilo.sh, served at https://tilo.sh)
# installs it: one archive per platform, tilo-<os>-<arch>.tar.gz holding the program `tilo`
# alone, the SHA-256 of each in SHA256SUMS, and SHA256SUMS.sig, a DER ECDSA P-256 signature of
# SHA256SUMS by the release key (keys/cosign.pub, AWS KMS alias/tilo-release-signing). It is
# published with the release tags (vX.Y.Z) that release the relay and the OS, so a command
# talks to the relay of its own release; `latest` follows the newest final version.
#
#   cli-release.sh build TARGET OUT    the program for TARGET, x86_64-unknown-linux-musl or
#                                      aarch64-unknown-linux-musl (statically linked, on a Linux
#                                      node of that architecture), aarch64-apple-darwin or
#                                      x86_64-apple-darwin (on a Mac), packed as OUT/tilo-<os>-
#                                      <arch>.tar.gz; holds no credentials
#   cli-release.sh pack PROGRAM ARCHIVE  PROGRAM packed as the archive ARCHIVE, as build packs it
#   cli-release.sh sums OUT DIR...     the archives of each DIR together in OUT, with SHA256SUMS;
#                                      both Linux archives are required
#   cli-release.sh sign DIR            DIR/SHA256SUMS signed with the release key in KMS into
#                                      DIR/SHA256SUMS.sig, checked the way the installer checks
#                                      it; with TILO_SIGN_TLOG=1 also entered in the public
#                                      transparency log, its entry kept as SHA256SUMS.bundle.
#                                      Releases only (a vX.Y.Z tag).
#   cli-release.sh publish DIR         the signed release to code.tilo.so's package registry as
#                                      tilo-cli/<version>/<file>, and as tilo-cli/latest/<file>
#                                      for a final version newer than latest's; tilo.sh serves
#                                      both at /releases/ (the web entry's Caddyfile)
#   cli-release.sh rehearse DIR        sign (a throwaway key) and publish (into DIR/registry)
#                                      the release in DIR as a tag would, then install it with
#                                      the installer from there and run what it installed
#
# With TILO_PUBLISH_DRY_RUN set to a directory, `sign` signs with a throwaway key made here
# (its public half kept as DIR/dry-run.pub) and `publish` writes DIR/releases/<version>/ and
# DIR/releases/latest/ there instead of uploading, for any reference; the version is the tag's,
# else v0.0.0. A signature with the key in KMS leaves DIR/dry-run.pub empty.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
usage='usage: cli-release.sh build TARGET OUT | pack PROGRAM ARCHIVE | sums OUT DIR... | sign DIR | publish DIR | rehearse DIR'
package=https://code.tilo.so/api/packages/tilo/generic/tilo-cli
linux_archives=(tilo-linux-x86_64.tar.gz tilo-linux-aarch64.tar.gz)
all_archives=("${linux_archives[@]}" tilo-macos-aarch64.tar.gz tilo-macos-x86_64.tar.gz)

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

# Pack the program $1 as the archive $2: one member, `tilo`, mode 0755, owned by root and dated
# the epoch, gzip without a name or time, so the same program always packs to the same bytes.
pack() {
  python3 -I - "$1" "$2" <<'PY'
import gzip, io, sys, tarfile
program, archive = sys.argv[1], sys.argv[2]
data = open(program, "rb").read()
raw = io.BytesIO()
with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
    info = tarfile.TarInfo("tilo")
    info.size, info.mode, info.mtime = len(data), 0o755, 0
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    tar.addfile(info, io.BytesIO(data))
with open(archive, "wb") as out, gzip.GzipFile(filename="", mode="wb", fileobj=out, mtime=0, compresslevel=9) as gz:
    gz.write(raw.getvalue())
PY
}

cmd_build() {
  local target=${1:?$usage} out=${2:?$usage} os arch flags
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
    [[ $(uname -s) == Linux && $(uname -m) == "$arch" ]] \
      || die "$target is built on a Linux node of its architecture, not on $(uname -s) $(uname -m)"
    # musl's C compiler for the C the program links (ring, SQLite), which the check's setup
    # installs: the C must be compiled against musl's headers, not the system's.
    command -v musl-gcc >/dev/null || die 'no musl-gcc: install it (the cli-ARCH check sets it up)'
    export "CC_${target//-/_}=musl-gcc"
    flags="$flags -C target-feature=+crt-static -C relro-level=full -C link-arg=-Wl,-z,noexecstack"
  else
    [[ $(uname -s) == Darwin ]] || die "$target is built on a Mac, not on $(uname -s)"
    rustup target add "$target" >/dev/null
    # The oldest macOS the program runs on, for Rust and the C cc-rs compiles alike.
    export MACOSX_DEPLOYMENT_TARGET=11.0
  fi
  RUSTFLAGS=$flags CARGO_PROFILE_RELEASE_STRIP=symbols CARGO_INCREMENTAL=0 \
    cargo build --locked --release --target "$target" -p tilo-cli --bin tilo
  local program=${CARGO_TARGET_DIR:-$repo_root/target}/$target/release/tilo
  [[ -f $program ]] || die "the build left no $program"
  if [[ $os == linux ]]; then
    # Static: no program interpreter, no shared library, so it runs on any Linux distribution.
    if LC_ALL=C readelf -lW "$program" | grep -q 'Requesting program interpreter'; then
      die "$program names a program interpreter: it is not statically linked"
    fi
    if LC_ALL=C readelf -dW "$program" | grep -q '(NEEDED)'; then
      die "$program needs shared libraries: $(readelf -dW "$program" | grep '(NEEDED)')"
    fi
  else
    # Only what every Mac has: the system library and Apple's frameworks.
    local foreign
    foreign=$(otool -L "$program" | tail -n +2 | awk '{print $1}' | grep -Ev '^(/usr/lib/|/System/Library/)' || true)
    [[ -z $foreign ]] || die "$program links libraries a Mac does not carry: $foreign"
  fi
  # The architecture it runs on is this one (a Mac runs x86_64 under Rosetta, when installed).
  if [[ $arch == $(uname -m) || ( $os == macos && $arch == aarch64 && $(uname -m) == arm64 ) ]]; then
    "$program" --help >/dev/null || die "$program --help failed"
  fi
  rm -rf "$out"
  mkdir -p "$out"
  pack "$program" "$out/tilo-$os-$arch.tar.gz"
  if command -v sha256sum >/dev/null; then
    (cd "$out" && sha256sum "tilo-$os-$arch.tar.gz")
  else
    (cd "$out" && shasum -a 256 "tilo-$os-$arch.tar.gz")
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
    [[ -f $out/$name ]] || die "no $name among $*: a release carries both Linux archives"
  done
  (cd "$out" && LC_ALL=C sha256sum tilo-*.tar.gz | LC_ALL=C sort -k2 > SHA256SUMS)
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
  printf 'signed SHA256SUMS of tilo %s\n' "$(version)"
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
    printf 'published tilo %s into %s\n' "$v" "$root"
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

cmd_rehearse() {
  local release=${1:?$usage} dir registry home installed
  registry=$scratch/cli-rehearsal
  dir=$scratch/cli-rehearsal-release
  rm -rf "$registry" "$dir"
  mkdir -p "$registry"
  cp -r "$release" "$dir"
  export TILO_PUBLISH_DRY_RUN=$registry
  cmd_sign "$dir"
  cmd_publish "$dir"
  local machine
  case $(uname -m) in x86_64) machine=x86_64 ;; aarch64) machine=aarch64 ;; *) die "no release for $(uname -m)" ;; esac
  local refs=("$(version)")
  if [[ -n $(final "$(version)") ]]; then refs+=(latest); fi
  for ref in "${refs[@]}"; do
    home=$scratch/cli-rehearsal-home
    rm -rf "$home"
    mkdir -p "$home"
    # The installer as tilo.sh serves it, from the published layout, with the throwaway key as
    # the mirror's: it checks the signature and the archive's sum, then installs.
    env -i PATH=/usr/bin:/bin HOME="$home" TILO_RELEASES="file://$registry/releases" \
      TILO_VERSION="$ref" TILO_RELEASE_KEY="$dir/dry-run.pub" sh dist/install/tilo.sh \
      || die "the installer refused the rehearsed release at $ref"
    installed=$home/.local/bin/tilo
    cmp --silent "$installed" <(tar -xzOf "$dir/tilo-linux-$machine.tar.gz" tilo) \
      || die "the installer installed something other than tilo-linux-$machine.tar.gz's program"
    HOME=$home "$installed" --help >/dev/null || die "the installed tilo does not run"
  done
  # Without the mirror's key only the release key is trusted, which did not sign this.
  home=$scratch/cli-rehearsal-home
  rm -rf "$home" && mkdir -p "$home"
  if env -i PATH=/usr/bin:/bin HOME="$home" TILO_RELEASES="file://$registry/releases" \
    sh dist/install/tilo.sh 2>/dev/null; then
    die 'the installer accepted a release the release key did not sign'
  fi
  [[ ! -e $home/.local/bin/tilo ]] || die 'the installer wrote a program it refused'
  rm -rf "$home" "$registry" "$dir"
  echo "the installer installs the rehearsed release $(version) and refuses it unsigned by the release key"
}

case ${1:-} in
  build) shift; cmd_build "$@" ;;
  pack) shift; pack "${1:?$usage}" "${2:?$usage}" ;;
  sums) shift; cmd_sums "$@" ;;
  sign) shift; cmd_sign "$@" ;;
  publish) shift; cmd_publish "$@" ;;
  rehearse) shift; cmd_rehearse "$@" ;;
  *) die "$usage" ;;
esac
