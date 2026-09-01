#!/usr/bin/env bash
# Build a self-contained host bundle on Linux or macOS. Run on the target OS;
# this deliberately does not cross-compile platform disk APIs.
set -euo pipefail

out="${1:-dist/qos-imager}"
cargo build --locked --release -p imager
mkdir -p "$out"

write_digest() {
  artifact="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$(dirname "$artifact")" && sha256sum "$(basename "$artifact")") >"$artifact.sha256"
  else
    (cd "$(dirname "$artifact")" && shasum -a 256 "$(basename "$artifact")") >"$artifact.sha256"
  fi
}

case "$(uname -s)" in
  Darwin)
    app="$out/qOS Imager.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp target/release/imager "$app/Contents/MacOS/qos-imager-bin"
    cp apps/imager/packaging/macos/qos-imager "$app/Contents/MacOS/qos-imager"
    chmod +x "$app/Contents/MacOS/qos-imager" "$app/Contents/MacOS/qos-imager-bin"
    cp apps/imager/packaging/macos/Info.plist "$app/Contents/Info.plist"
    ditto -c -k --keepParent "$app" "$out/qos-imager-macos.zip"
    write_digest "$out/qos-imager-macos.zip"
    ;;
  Linux)
    bundle="$out/qos-imager-linux"
    mkdir -p "$bundle"
    cp target/release/imager "$bundle/qos-imager-bin"
    cp apps/imager/packaging/linux/qos-imager "$bundle/qos-imager"
    chmod +x "$bundle/qos-imager" "$bundle/qos-imager-bin"
    cp apps/imager/packaging/linux/qos-imager.desktop "$bundle/"
    cp docs/installer-walkthrough.md "$bundle/README.md"
    tar -C "$out" -czf "$out/qos-imager-linux.tar.gz" qos-imager-linux
    write_digest "$out/qos-imager-linux.tar.gz"
    ;;
  *)
    printf 'unsupported host: %s\n' "$(uname -s)" >&2
    exit 2
    ;;
esac
