#!/usr/bin/env bash
# Each builder crate as someone adds it from crates.io, alone, in an empty project outside the
# repository: `cargo add NAME@=VERSION` with its default features, then `cargo build`. Run by t's
# published-crates-standalone check once a release is out (`t dispatch published-crates-standalone
# --ref main --repo tilo`, `--input RELEASE=vX.Y.Z` for a release other than the one
# tools/ci/publish/release-version names).
#
# Inside the workspace, features unify across every member, so a crate whose default build names
# an optional dependency still compiles there; `publish-crates-dry-run` builds each package on its
# own before the tag, and this builds what crates.io serves after it.
#
# Every crate is built, and each one that failed is named at the end.
set -uo pipefail

crates=(
  tilo-ui tilo-ui-base tilo-ui-app
  tilo-app tilo-resource tilo-view tilo-history tilo-doc
  tilolang tilolang-runtime tilolang-lex tilolang-cli tilolang-web
)

release=${T_INPUT_RELEASE:-}
[[ -n $release ]] || release=v$(tr -d '[:space:]' < tools/ci/publish/release-version)
version=${release#v}
# Nothing of the job's build environment: a project of someone else's sees Cargo's defaults.
for name in $(compgen -e); do
  case $name in
    CARGO*|RUSTFLAGS|RUSTDOCFLAGS|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|SCCACHE*) unset "$name" ;;
  esac
done
# Out of /tmp, whose Cargo configuration t keeps offline, and with crates.io reachable: the
# sandbox's /.cargo/config.toml, which Cargo reads from any directory, keeps it offline too.
export CARGO_NET_OFFLINE=false
work=$(mktemp -d -p /var/tmp)
trap 'rm -rf -- "$work"' EXIT
export CARGO_HOME=$work/cargo-home CARGO_TARGET_DIR=$work/target
failed=()

for crate in "${crates[@]}"; do
  printf '== %s %s\n' "$crate" "$version"
  project=$work/uses-${crate}
  if cargo new --quiet --lib --vcs none "$project" \
    && (cd "$project" && cargo add --quiet "$crate@=$version" && cargo build --quiet); then
    printf 'ok: %s\n' "$crate"
  else
    printf 'FAILED: %s\n' "$crate"
    failed+=("$crate")
  fi
done

if ((${#failed[@]})); then
  printf '%d of %d crates at %s do not build alone with their default features:\n' \
    "${#failed[@]}" "${#crates[@]}" "$version"
  printf '  %s\n' "${failed[@]}"
  exit 1
fi
printf 'every crate at %s builds alone, from crates.io, with its default features\n' "$version"
