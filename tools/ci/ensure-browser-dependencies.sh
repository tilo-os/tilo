#!/usr/bin/env bash
# Current builders contain these libraries. Avoid consulting DNF repositories
# on every browser job when the complete dependency set is already installed.
set -euo pipefail

packages=(
  nss atk at-spi2-atk cups-libs libdrm libXcomposite libXdamage
  libXrandr mesa-libgbm pango alsa-lib libxkbcommon
)

if rpm -q "${packages[@]}" >/dev/null; then
  echo "Browser dependencies are installed."
else
  dnf install -y "${packages[@]}"
fi
