#!/usr/bin/env bash
# Keys remain at the isolated signing service; the job receives public artifacts.
set -euo pipefail
exec python3 "$(dirname -- "$0")/sign-rpms-broker.py" "$@"
