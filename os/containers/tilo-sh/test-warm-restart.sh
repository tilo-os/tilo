#!/usr/bin/env bash
# A warm `stack.sh restart web` after a change to one crate serves a page
# whose split modules link: the script changes the crate, restarts the web
# entry, waits for the site to serve the new bundle, and then fetches the
# start module and every module `split.json` names and instantiates them
# together in node, the way `pkg/__wasm_split.js` links them. A split module
# the start module does not answer — or answers with another signature, the
# browser's "function signature mismatch" — fails here rather than in the
# page.
#
# usage: test-warm-restart.sh [--change FILE] [--from TEXT] [--to TEXT]
#                             [--site URL] [--no-change]
#
# --change FILE  the crate file the script edits (default
#                apps/weather/src/freshness.rs)
# --from TEXT    the line in it the script changes (default the weather app's
#                `RECENT_SECS`, one constant and nothing else); --to TEXT is
#                what it becomes. The file is restored when it ends
# --site URL     the stack's site (default http://tilo.localhost:BASE)
# --no-change    restart without a change, and check the bundle it serves:
#                what a warm restart costs when nothing reaches a module
#
# The stack is the one stack.sh's environment names (TILO_SH_STATE,
# TILO_SH_PORT_BASE, TILO_SH_CARGO, ...); it comes up with `stack.sh up`.
# Needs node (v20 or later), curl, and the checkout clean in FILE.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
base=${TILO_SH_PORT_BASE:-24100}
change=$repo/apps/weather/src/freshness.rs
from='const RECENT_SECS: i64 = 60 * 60;'
to=
site=http://tilo.localhost:$((base + 20))
change_it=1
usage='usage: test-warm-restart.sh [--change FILE] [--from TEXT] [--to TEXT] [--site URL] [--no-change]'
while [[ $# -gt 0 ]]; do
  case "$1" in
    --change) change=${2:?--change needs a file}; shift 2 ;;
    --from) from=${2:?--from needs a line}; shift 2 ;;
    --to) to=${2:?--to needs a line}; shift 2 ;;
    --site) site=${2:?--site needs a URL}; shift 2 ;;
    --no-change) change_it=0; shift ;;
    -h|--help) printf '%s\n' "$usage"; exit 0 ;;
    *) printf '%s\n' "$usage" >&2; exit 2 ;;
  esac
done
die() { printf 'test-warm-restart: %s\n' "$*" >&2; exit 1; }
# The default line the script changes: the weather app's recent-shelf, to a
# value of its own, so the bundle is never the one the time before it left.
[[ $change_it == 0 || -n $to ]] ||
  to="const RECENT_SECS: i64 = $((60 + (RANDOM % 900))) * 60;"
[[ -f $change ]] || die "$change is no file of this checkout"
[[ -z $(git -C "$repo" status --porcelain -- "$change") ]] ||
  die "$change has changes of its own; the script edits and restores it"
command -v node >/dev/null || die 'needs node, which instantiates the modules the site serves'
stack() { bash "$here/stack.sh" "$@"; }

# The bundle the site serves: the entry page names its version directory.
served() {
  curl -fsS "$site/app/index.html" | sed -n 's|.*"\.\./v/\([0-9a-f]*\)/.*|\1|p' | head -n 1
}
before=$(served) || true
[[ -n ${before:-} ]] || die "the site at $site serves no entry page (is the stack up?)"

# Every module of the bundle the site serves, instantiated together the way
# the page's loader links them: the start module first, then each shared
# chunk, then each app module, against the exports of those before it.
work=$(mktemp -d)
trap 'rm -rf "$work"; if [[ $change_it == 1 ]]; then git -C "$repo" checkout -- "$change"; fi' EXIT
cat > "$work/check.mjs" <<'EOF'
const base = process.argv[2];
const manifest_response = await fetch(`${base}/pkg/split.json`);
if (!manifest_response.ok) throw new Error(`${base}/pkg/split.json: HTTP ${manifest_response.status}`);
const manifest = await manifest_response.json();
const load = async (file) => {
  const response = await fetch(`${base}/pkg/${file}`);
  if (!response.ok) throw new Error(`${file}: HTTP ${response.status}`);
  return new WebAssembly.Module(await response.arrayBuffer());
};
// The pages' own functions, which the start module imports: any JavaScript
// function stands in for them, as it does in the page.
const stubs = (imports) => {
  const object = {};
  for (const {module, name, kind} of imports) {
    const fields = (object[module] ??= {});
    if (kind === 'function') fields[name] = () => 0;
    else if (kind === 'memory') fields[name] = new WebAssembly.Memory({initial: 1});
    else if (kind === 'table') fields[name] = new WebAssembly.Table({element: 'anyfunc', initial: 1});
    else if (kind === 'global') fields[name] = new WebAssembly.Global({value: 'i32'}, 0);
  }
  return object;
};
const main = await load(manifest.main.file);
const start = await WebAssembly.instantiate(main, stubs(WebAssembly.Module.imports(main)));
// What the glue passes every module: the start module's memory, and its
// exports beside those of the modules loaded before (pkg/__wasm_split.js).
const linked = {};
const load_with = async (file) => {
  const imports = {__wasm_split: {...start.exports, ...linked}};
  if (start.exports.memory !== undefined) imports.env = {memory: start.exports.memory};
  try {
    Object.assign(linked, (await WebAssembly.instantiate(await load(file), imports)).exports);
  } catch (error) {
    throw new Error(`${file}: ${error.message}`);
  }
};
const shared = new Set(manifest.shared.map((entry) => entry.file));
for (const file of shared) await load_with(file);
for (const module of manifest.modules) {
  for (const file of module.shared) {
    if (!shared.has(file)) throw new Error(`${module.module} needs ${file}, which the manifest does not name`);
  }
}
for (const module of manifest.modules) await load_with(module.file);
const modules = shared.size + manifest.modules.length;
console.log(`ok: the start module and ${modules} split modules link (${manifest.modules.length} app modules)`);
EOF

if [[ $change_it == 1 ]]; then
  # The line the script changes, and only it: a file whose line the checkout
  # moved is one the runner names with --from and --to.
  [[ $(grep -cxF -- "$from" "$change") == 1 ]] ||
    die "$change holds $from not once; name the line to change with --from and --to"
  python3 -I - "$change" "$from" "$to" <<'PY'
import sys
path, old, new = sys.argv[1:]
text = open(path, encoding="utf-8").read()
assert text.count(old) == 1, f"{path} holds {old!r} {text.count(old)} times"
open(path, "w", encoding="utf-8").write(text.replace(old, new))
PY
  printf 'test-warm-restart: changed %s\n' "${change#"$repo/"}" >&2
else
  printf 'test-warm-restart: no change (the warm restart as it is)\n' >&2
fi
began=$SECONDS
stack restart web >&2 || die 'the stack did not restart the web entry (above)'
took=$((SECONDS - began))
after=$(served) || after=$before
if [[ $change_it == 1 ]]; then
  for _ in $(seq 1 150); do
    after=$(served) || after=$before
    [[ $after != "$before" ]] && break
    sleep 2
  done
  [[ $after != "$before" ]] ||
    die "the site still serves the bundle it served before ($before); the change reached no module of it"
fi
node "$work/check.mjs" "$site/v/$after" ||
  die "the bundle the site serves ($after) does not link"
printf 'test-warm-restart: web restarted in %ss, serving %s (was %s)\n' "$took" "$after" "$before"
