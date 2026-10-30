#!/usr/bin/env bash
# Wipe the documents-and-forms trial for a fresh attempt.
#
#   ./reset.sh
#
# Stops the portal, deletes its state (applications and uploads) and the whole
# ~/Documents/Trial folder (Sources, Out, Submission and anything the last
# participant left there), writes the source documents again and starts the
# portal. It does not touch the desktop itself: close the last attempt's
# windows and revoke its agent before the next participant starts.
set -euo pipefail

unit=tilo-trial-portal.service
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
state="$HOME/.local/state/tilo-trial/documents-and-forms"
trial="$HOME/Documents/Trial"

# Both paths are fixed below $HOME; refuse anything else before deleting.
for path in "$state" "$trial"; do
  case "$path" in
    "$HOME"/?*) ;;
    *) echo "reset.sh: refusing to delete $path" >&2; exit 1 ;;
  esac
done

running=0
if systemctl --user is-active --quiet "$unit" 2>/dev/null; then
  running=1
  systemctl --user stop "$unit"
fi

rm -rf -- "$state" "$trial"
mkdir -p "$state"
python3 -s -E "$here/generate.py" "$trial/Sources" >/dev/null
mkdir -p "$trial/Out" "$trial/Submission"
echo "Wrote fresh source documents to $trial/Sources and emptied the portal's state"

if [[ "$running" == 1 ]] || systemctl --user is-enabled --quiet "$unit" 2>/dev/null; then
  systemctl --user start "$unit"
  echo "The portal is running again at http://127.0.0.1:8471/"
fi
