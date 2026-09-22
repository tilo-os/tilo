#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
app_cli="$root/crates/packaging/app-cli/src/install.rs"
app_host="$root/crates/packaging/app-host/src/service.rs"

for source in "$app_cli" "$app_host"; do
  grep -Eq 'bounded_command::(run_prepared|spawn_prepared)\(' "$source"
  if grep -Eq '\.output\(\)|wait_with_output\(' "$source"; then
    echo "unbounded packaging subprocess in ${source#"$root/"}" >&2
    exit 1
  fi
done

image="$root/os/Containerfile"
grep -Fq -- '-p app-cli' "$image"
grep -Fq -- '--bin app' "$image"
grep -Fq 'ln -s app /out/store' "$image"
grep -Fq 'ln -s tilo /out/system' "$image"
grep -Fq 'target/release/tilo completions bash' "$image"
grep -Fq 'target/release/app completions fish' "$image"
grep -Fq '/usr/share/bash-completion/completions/' "$image"
grep -Fq '/usr/share/fish/vendor_completions.d/' "$image"
grep -Fq '/usr/share/zsh/site-functions/' "$image"

echo 'packaging subprocess boundary: ok'
