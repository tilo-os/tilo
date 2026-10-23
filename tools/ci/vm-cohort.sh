#!/usr/bin/env bash
# The prebuilt layer cohort vm-acceptance-cohort installs through verified
# swap-artifact (docs/layer-artifacts.md), as two t checks:
#
#   vm-cohort.sh produce   vm-cohort-x86_64: this commit's launcher, install,
#                          shell and apps+runtime as full signed layer streams,
#                          packed by this tree's tilo-vm (tilo-vm-binary-x86_64)
#                          through scripts/vm-acceptance/layer_artifacts.py, into
#                          /target/vm-cohort/cohort with the handoff's identity in
#                          /target/vm-cohort/handoff.json
#   vm-cohort.sh verify    vm-cohort-verifier-x86_64: builds tilo-vm from main's
#                          head, fetched from the forge, as the cohort's verifier
#                          and consumer (/target/vm-cohort-verifier/tilo-vm-artifact),
#                          and checks the handoff with main's layer_artifacts.py
#
# The verifier is main's code, not this tree's: the code under test never
# verifies its own artifacts, and a tree older than a consumer fix stays
# testable. Producing needs an exact clean commit (the receipt names it), so
# it runs for a pushed commit: `t test` of a clean checkout, or `t dispatch`.
set -euo pipefail

mode=${1:?usage: vm-cohort.sh produce|verify}
arch=x86_64
cohort=/target/vm-cohort
verifier=/target/vm-cohort-verifier
# The cohort's components: apps+runtime brings launcher, install and shell with it.
components=apps+runtime
forge=https://code.tilo.so/tilo/tilo.git

case $mode in
  produce)
    source=$(git rev-parse --verify HEAD)
    builder=$(python3 -c 'import tomllib; print(tomllib.load(open("t.toml", "rb"))["image"])')
    # t numbers no runs: a random handoff number keeps every production's identity its own.
    run_id=$(python3 -c 'import secrets; print(secrets.randbelow(2**53 - 1) + 1)')
    rm -rf "$cohort"
    mkdir -p "$cohort"
    digest=$(python3 scripts/vm-acceptance/layer_artifacts.py produce \
      --repo . --directory "$cohort/cohort" --source "$source" --builder-image "$builder" \
      --run-id "$run_id" --run-attempt 1 --components "$components" --packer /target/tilo-vm-bin/tilo-vm)
    python3 - "$source" "$builder" "$run_id" "$components" "$digest" "$arch" > "$cohort/handoff.json" <<'PY'
import json, sys
source, builder, run, components, digest, arch = sys.argv[1:]
print(json.dumps(dict(source=source, builder_image=builder, run_id=int(run), run_attempt=1,
                      components=components, receipt_sha256=digest, architecture=arch), indent=1))
PY
    cat "$cohort/handoff.json"
    ;;
  verify)
    handoff=$cohort/handoff.json
    [[ -s $handoff ]] || { echo "no cohort: the check needs vm-cohort-$arch" >&2; exit 1; }
    main=$(git ls-remote "$forge" refs/heads/main | cut -f1)
    [[ $main =~ ^[0-9a-f]{40}$ ]] || { echo "could not resolve main at $forge" >&2; exit 1; }
    rm -rf "$verifier"
    mkdir -p "$verifier"
    src=${TMPDIR:-/tmp}/verifier-src
    rm -rf "$src"
    git init -q "$src"
    git -C "$src" fetch -q --depth=1 --no-tags "$forge" "$main"
    git -C "$src" -c advice.detachedHead=false checkout -q FETCH_HEAD
    (cd "$src" && CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$src/target}/vm-cohort-verifier \
      cargo build --locked -p tilo-vm --bin tilo-vm)
    install -m 0755 "${CARGO_TARGET_DIR:-$src/target}/vm-cohort-verifier/debug/tilo-vm" "$verifier/tilo-vm-artifact"
    "$verifier/tilo-vm-artifact" swap-artifact --help >/dev/null
    field() { python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$handoff" "$1"; }
    python3 "$src/scripts/vm-acceptance/layer_artifacts.py" verify --directory "$cohort/cohort" \
      --source "$(field source)" --builder-image "$(field builder_image)" \
      --run-id "$(field run_id)" --run-attempt "$(field run_attempt)" \
      --components "$(field components)" --receipt-sha256 "$(field receipt_sha256)"
    printf '{"commit": "%s"}\n' "$main" > "$verifier/verifier.json"
    echo "the cohort of $(field source) verifies with main $main's tilo-vm"
    ;;
  *) echo "usage: vm-cohort.sh produce|verify" >&2; exit 2 ;;
esac
