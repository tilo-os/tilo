#!/usr/bin/env bash
# Key-to-present latency of typing in a tilolang app, on the session build
# TILO_NESTED_SESSION_BUILD names, which this never builds (dist/dev/nested-session-build.sh):
# the app's package is built from the tree, installed on the headless session as a person's
# device installs it, and typed into (scripts/perf/headless_key_latency.py).
#
#   tools/ci/key-latency.sh OUT APP ACTION [headless_key_latency.py args...]
#
# APP is a package under apps/tl (notes), ACTION the app's action that opens something to type
# into ("New note"). OUT gets the result (APP.json), its table and the session's logs.
set -euo pipefail

out=${1:?usage: $0 OUT_DIR APP ACTION [args...]}
app=${2:?usage: $0 OUT_DIR APP ACTION [args...]}
action=${3:?usage: $0 OUT_DIR APP ACTION [args...]}
shift 3
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
cd "$repo_root"

target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
build=$(realpath -m "${TILO_NESTED_SESSION_BUILD:-$target/nested-session}")
[[ -x $build/bin/tilo-session ]] || {
    echo "no session build in $build (dist/dev/nested-session-build.sh)" >&2
    exit 1
}
export TILO_NESTED_SESSION_BUILD=$build
export PYTHONDONTWRITEBYTECODE=1

# The session runs on the seat a device gives a person, as the app walks' do.
# shellcheck source=tools/ci/seat.sh
source "$repo_root/tools/ci/seat.sh"
seat_up

out=$(realpath -m "$out")
# The session's state, on a tree its account can read all the way up (tools/ci/app-walk.sh).
base=$(mktemp -d -p /var/tmp tilo-key-latency.XXXXXXXX)
chmod 0755 "$base"
rm -rf -- "$out"
mkdir -p -- "$out"
finish() {
    seat_down
    local log
    for log in "$base"/run/logs/*.log "$base"/state/logs/*.log; do
        [[ -f $log ]] && cp "$log" "$out/"
    done
    rm -rf -- "$base"
}
trap finish EXIT

tapp=$(python3 -I -B - "$app" "$base" <<'PY'
import importlib.util, pathlib, sys
repo = pathlib.Path.cwd()
sys.path.insert(0, str(repo / "scripts/headless-examples"))
import real_apps
spec = importlib.util.spec_from_file_location("app_walk", repo / "scripts/headless-examples/app-walk.py")
walks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(walks)
app, base = sys.argv[1], pathlib.Path(sys.argv[2])
state = base / "build-state"
real_apps.prepare(state, apps="", built=False)
print(walks.build_tapp(app, base / "build", state))
PY
)
# The app's id is what its package declares (`app so.tilo.notes "Notes"`).
id=$(sed -n 's/^app[[:space:]]\{1,\}\([^[:space:]]*\).*/\1/p' "apps/tl/$app/app.tl" | head -1)
python3 -I -B scripts/perf/headless_key_latency.py --label "$app" --app "$id" --apps "" \
    --tapp "$tapp" --open "$action" --click-at 0.5,0.17 \
    --state "$base/state" --out "$base/run" "$@" | tee "$out/table.md"
cp "$base"/run/*.json "$out/"
