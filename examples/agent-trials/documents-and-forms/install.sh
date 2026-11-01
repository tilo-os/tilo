#!/usr/bin/env bash
# Install the documents-and-forms agent trial for the account that runs it.
#
#   ./install.sh [--no-service]
#
# Copies the pack to ~/.local/share/tilo-trial/documents-and-forms, writes the
# source documents to ~/Documents/Trial/Sources (with empty Out and Submission
# folders beside them), and runs the mock portal as a systemd user service on
# http://127.0.0.1:8471/. Run it again to update the pack; it keeps the
# participant's outputs and the portal's state. reset.sh starts a fresh
# attempt. Needs python3 3.9 or newer and nothing else.
set -euo pipefail

port=8471
unit=tilo-trial-portal.service
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dest="$HOME/.local/share/tilo-trial/documents-and-forms"
state="$HOME/.local/state/tilo-trial/documents-and-forms"
trial="$HOME/Documents/Trial"
service=1

for arg in "$@"; do
  case "$arg" in
    --no-service) service=0 ;;
    -h | --help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "install.sh: unknown argument $arg" >&2; exit 2 ;;
  esac
done

python=$(command -v python3 || true)
if [[ -z "$python" ]] || ! "$python" -c 'import sys; sys.exit(sys.version_info < (3, 9))'; then
  echo "install.sh: python3 3.9 or newer is required" >&2
  exit 1
fi

mkdir -p "$dest" "$state"
if [[ "$here" != "$dest" ]]; then
  for file in facts.py pdfmini.py generate.py portal.py check.py install.sh reset.sh README.md TASKS.md; do
    install -m 0644 "$here/$file" "$dest/$file"
  done
  chmod 0755 "$dest/install.sh" "$dest/reset.sh" "$dest/generate.py" "$dest/portal.py" "$dest/check.py"
fi

"$python" -s -E "$dest/generate.py" "$trial/Sources" >/dev/null
mkdir -p "$trial/Out" "$trial/Submission"
echo "Source documents are in $trial/Sources"

if [[ "$service" == 0 ]]; then
  echo "Start the portal yourself with:"
  echo "  $python -s -E $dest/portal.py --state-dir $state --port $port"
  exit 0
fi

units="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$units"
cat >"$units/$unit" <<EOF
[Unit]
Description=Mock permit portal for the documents-and-forms agent trial

[Service]
ExecStart=$python -s -E $dest/portal.py --state-dir $state --port $port
Restart=on-failure
NoNewPrivileges=yes

[Install]
WantedBy=default.target
EOF

if ! systemctl --user daemon-reload; then
  echo "install.sh: this login has no systemd user manager. Run install.sh from a session on the" >&2
  echo "desktop, or ask an administrator to run: loginctl enable-linger $USER" >&2
  exit 1
fi
systemctl --user enable "$unit" >/dev/null
systemctl --user restart "$unit"

"$python" - "$port" <<'EOF'
import sys, time, urllib.request
url = f"http://127.0.0.1:{sys.argv[1]}/healthz"
for _ in range(50):
    try:
        with urllib.request.urlopen(url, timeout=1) as response:
            if response.read().strip() == b"ok":
                sys.exit(0)
    except OSError:
        time.sleep(0.2)
sys.exit(f"the portal did not answer at {url}; see: journalctl --user -u tilo-trial-portal")
EOF
echo "The portal answers at http://127.0.0.1:$port/"
echo "Check the tasks with: python3 $dest/check.py"
