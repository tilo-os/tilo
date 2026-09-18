#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
catalogue="$root/crates/packaging/app-catalogue/src/install.rs"
app_host="$root/crates/packaging/app-host/src/service.rs"

for source in "$catalogue" "$app_host"; do
  grep -Fq 'bounded_command::run_prepared(' "$source"
  if grep -Eq '\.output\(\)|wait_with_output\(' "$source"; then
    echo "unbounded packaging subprocess in ${source#"$root/"}" >&2
    exit 1
  fi
done

echo 'packaging subprocess boundary: ok'
