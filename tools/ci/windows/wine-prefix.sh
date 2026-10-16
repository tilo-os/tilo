#!/usr/bin/env bash
# Create a Wine prefix at $WINEPREFIX that runs Windows programs without a
# display: checks have no X or Wayland server, and Wine without a graphics
# driver fails every window it is asked for, which installers and GUI
# programs create even when silent or offscreen. The null driver gives them
# windows that exist but are never shown.
#
# Wine Mono and Gecko are not installed (nothing here is .NET or HTML), so
# Wine never offers to download them. The driver is read when Wine's desktop
# starts, so the prefix's processes are stopped before the caller runs
# anything in it.
set -euo pipefail
: "${WINEPREFIX:?WINEPREFIX names the prefix to create}"
export WINEDEBUG="${WINEDEBUG:--all}" WINEDLLOVERRIDES="${WINEDLLOVERRIDES:-mscoree,mshtml=}"
mkdir -p "$WINEPREFIX"
wine wineboot --init >/dev/null 2>&1
wine reg add 'HKCU\Software\Wine\Drivers' /v Graphics /d null /f >/dev/null
wineserver -w
