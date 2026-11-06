#!/usr/bin/env bash
# Replace the pinned Rust toolchain's cargo with the same release built from its
# upstream source and the patches in os/external-assets/cargo/ (README.md there
# says why). rustc and every other component stay the toolchain's; `cargo`
# still resolves through rustup to the toolchain's bin/cargo.
#
# The caller has installed the toolchain rust-toolchain.toml pins (whose cargo
# builds this one), fetched the source into the asset cache, and provides gcc,
# make and perl for the vendored C libraries; the build downloads crates.
#
#   install-cargo.sh CACHE ASSET_MANIFEST PATCH_DIR
#   install-cargo.sh --asset-name
#
# Delete this script, os/external-assets/cargo/ and their callers when
# rust-toolchain.toml moves to 1.100 or later, whose cargo carries the fix.
set -euo pipefail

# cargo 1.97.1, as Rust 1.97.1 ships it: the commit its `cargo -vV` names.
asset=cargo-c980f48.tar.gz
base=c980f4866141969fab6254a680546a277789d6f0
base_date=2026-06-30
release=1.97.1
source_url=https://github.com/rust-lang/cargo/tree/$base

usage() {
  echo 'usage: install-cargo.sh CACHE ASSET_MANIFEST PATCH_DIR | install-cargo.sh --asset-name' >&2
  exit 2
}

if [[ $# == 1 && $1 == --asset-name ]]; then
  printf '%s\n' "$asset"
  exit 0
fi
[[ $# == 3 ]] || usage
cache=$1
manifest=$2
patches=$(cd "$3" && pwd)

# Only the cargo this source is: a toolchain that moved on would get an older
# cargo than its own.
installed=$(rustup which cargo)
"$installed" -vV | grep -Fqx "commit-hash: $base" || {
  echo "the toolchain's cargo is not $release ($base): rust-toolchain.toml moved." >&2
  echo 'From 1.100 its cargo carries the fix: delete tools/ci/install-cargo.sh and its callers.' >&2
  exit 1
}

expected=$(awk -F '\t' -v name="$asset" '$1 !~ /^#/ && $2 == name {print $1}' "$manifest")
[[ $expected =~ ^[0-9a-f]{64}$ ]] || { echo "missing or ambiguous digest for $asset" >&2; exit 1; }
printf '%s  %s\n' "$expected" "$asset" | (cd "$cache" && sha256sum --check --strict -) >&2

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/src" "$stage/smoke"
tar -xzf "$cache/$asset" --strip-components=1 --no-same-owner -C "$stage/src"
# The ceiling keeps git apply from treating an enclosing checkout as the tree to patch.
for patch in "$patches"/*.patch; do
  (cd "$stage/src" && GIT_CEILING_DIRECTORIES="$stage" git apply --whitespace=nowarn "$patch")
done
(
  cd "$stage/src"
  # What Rust's own build gives cargo, so it reports the toolchain's version and commit;
  # the description says it is this build. all-static links libgit2, curl and its TLS
  # library in, and LIBZ_SYS_STATIC zlib, as the upstream binary does.
  printf '%s\n%s\n%s\n' "$base" "${base:0:9}" "$base_date" > git-commit-info
  # The toolchain's own compiler: through rustup's proxy, a crate that ships a
  # rust-toolchain file (ff does) would be compiled by the toolchain it names.
  export RUSTC="$(dirname "$installed")/rustc" \
    CFG_RELEASE=$release CFG_RELEASE_CHANNEL=stable LIBZ_SYS_STATIC=1 \
    CFG_VER_DESCRIPTION='tilo: rust-lang/cargo#17412' \
    CARGO_TARGET_DIR="$stage/target" \
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$stage=/cargo"
  "$installed" build --locked --release -p cargo --features all-static
)
built=$stage/target/release/cargo
"$built" --version | grep -Fqx "cargo $release (${base:0:9} $base_date) (tilo: rust-lang/cargo#17412)"

# It builds, and still writes an SBOM where one is asked for.
(
  cd "$stage/smoke"
  printf '[package]\nname = "smoke"\nversion = "0.1.0"\nedition = "2021"\n' > Cargo.toml
  mkdir src && echo 'fn main() {}' > src/main.rs
  export CARGO_TARGET_DIR="$stage/smoke/target" RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER=
  "$built" build --offline --quiet
  [[ -z $(find target -name '*.cargo-sbom.json') ]]
  RUSTC_BOOTSTRAP=1 CARGO_BUILD_SBOM=true "$built" -Zsbom build --offline --quiet --release
  [[ -n $(find target/release -name '*.cargo-sbom.json') ]]
)

install -m 0755 "$built" "$installed"
# The license of the toolchain's cargo covers this build; the patches it carries
# and its source sit beside it.
doc=$(dirname "$(dirname "$installed")")/share/doc/cargo/tilo
install -D -m 0644 -t "$doc" "$patches"/*.patch
printf 'Source: %s (%s), sha256 %s\nModified: bin/cargo is built from it with the patches here.\n' \
  "$source_url" "$asset" "$expected" > "$doc/SOURCE"
cargo --version
