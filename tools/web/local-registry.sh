#!/usr/bin/env bash
# A throwaway signed registry on loopback serving web builds, for developing
# the browser runtime against the real publish/review/sign/verify path.
#
#   tools/web/local-registry.sh [options] BUILD_DIR
#
# BUILD_DIR is the output of tilo-browser-runtime's build-web-dev.sh: the
# core (pkg/, runtime.json) and apps (apps/index.json, apps/<id>/app.json and
# app.wasm). Each run, under $TILO_LOCAL_REGISTRY_DIR (default
# .local-registry/ in the checkout, gitignored):
#
#   1. makes fresh owner and publisher keys (keys/) and a fresh registry
#      (data/) whose base URL is http://127.0.0.1:PORT;
#   2. packs the core (kind=runtime, tools/web/web-core.tilo-app.toml) and
#      every app with `tilo-app-package pack-web`, using each app's
#      apps/<package>/tilo-app.toml;
#   3. hosts each tapp (`import-artifact`), submits its signed entry
#      (POST /entries), approves it (`review approve`) and signs a snapshot;
#   4. writes registry.json ({baseUrl, ownerKeyHex, runtimeAbi, apps}) for
#      the page (also into BUILD_DIR, for its registry.html) and, with
#      --check, resolves everything through
#      registry-client.mjs and the real verifier in Node;
#   5. keeps serving (GET-only CORS for each --origin) until Ctrl-C, unless
#      --check was given.
#
# Options:
#   --port PORT        listen port (default 8787)
#   --origin ORIGIN    page origin allowed to read cross-origin; repeatable
#                      (default http://localhost:4321)
#   --app MANIFEST=WASM  also publish this app module; repeatable
#   --check            verify end to end in Node, then stop the server
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
state=${TILO_LOCAL_REGISTRY_DIR:-$repo/.local-registry}
target_dir=${CARGO_TARGET_DIR:-$repo/target}
port=8787
origins=()
extra_apps=()
check=0
build=""

while (($#)); do
  case $1 in
    --port) port=$2; shift 2 ;;
    --origin) origins+=("$2"); shift 2 ;;
    --app) extra_apps+=("$2"); shift 2 ;;
    --check) check=1; shift ;;
    -h|--help) sed -n '2,31p' "$0"; exit 0 ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) build=$(cd "$1" && pwd); shift ;;
  esac
done
[[ -n $build || ${#extra_apps[@]} -gt 0 ]] || { sed -n '2,31p' "$0"; exit 2; }
((${#origins[@]})) || origins=(http://localhost:4321)
base="http://127.0.0.1:$port"

cargo build --locked --manifest-path "$repo/Cargo.toml" -j "${CARGO_BUILD_JOBS:-3}" \
  -p registry-server -p tilo-app-package-cli --bins
registry=$target_dir/debug/tilo-registry-server
tapp=$target_dir/debug/tilo-app-package

rm -rf "$state/data" "$state/tapps" "$state/requests"
mkdir -p "$state/keys" "$state/data" "$state/tapps" "$state/requests"
chmod 700 "$state/keys"
reg() { "$registry" --data-dir "$state/data" "$@"; }
json() { node -e "$1" "${@:2}"; }

# Ed25519 public key (hex) of a 32-byte seed (hex).
pubkey_of() {
  json 'const {createPrivateKey, createPublicKey} = require("node:crypto");
    const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(process.argv[1], "hex")]);
    const jwk = createPublicKey(createPrivateKey({key: der, format: "der", type: "pkcs8"})).export({format: "jwk"});
    console.log(Buffer.from(jwk.x, "base64url").toString("hex"));' "$1"
}

echo "== keys"
(umask 077; head -c 32 /dev/urandom | od -An -vtx1 | tr -d ' \n' > "$state/keys/owner.seed")
owner_hex=$(pubkey_of "$(cat "$state/keys/owner.seed")")
rm -f "$state/keys/publisher.json"
"$tapp" keygen --out "$state/keys/publisher.json" >/dev/null
publisher_hex=$(pubkey_of "$(json 'console.log(require(process.argv[1]).secret_key_hex)' "$state/keys/publisher.json")")
echo "owner     $owner_hex"
echo "publisher $publisher_hex"

echo "== registry at $base"
reg init --owner-pubkey "$owner_hex" --name "Local web registry" \
  --description "Throwaway registry for browser runtime development" --base-url "$base" >/dev/null
reg add-publisher --pubkey "$publisher_hex" --can-add-new --note "local web builds" >/dev/null
cors=()
for origin in "${origins[@]}"; do cors+=(--cors-origin "$origin"); done
if curl -fsS "$base/health" >/dev/null 2>&1; then
  echo "something already serves $base; stop it or pass --port" >&2
  exit 1
fi
reg serve --port "$port" "${cors[@]}" > "$state/serve.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
trap 'exit 130' INT TERM
for _ in $(seq 50); do curl -fsS "$base/health" >/dev/null 2>&1 && break; sleep 0.1; done
kill -0 "$server" 2>/dev/null || { cat "$state/serve.log" >&2; exit 1; }
curl -fsS "$base/health" >/dev/null

# publish TAPP PACKAGE_ID: host the bytes, submit, approve.
publish() {
  local file=$1 id=$2 url
  url=$(reg import-artifact "$file")
  "$tapp" entry --url "$url" --identity "$state/keys/publisher.json" \
    --platform web --arch wasm32 --action add "$file" > "$state/requests/$id.json"
  curl -fsS -H 'content-type: application/json' --data-binary "@$state/requests/$id.json" \
    "$base/entries" >/dev/null
  reg review approve "$id" --reviewer local --reason "local web build" \
    --allow-insecure-local >/dev/null
  echo "published $id  $(stat -c %s "$file") bytes  $url"
}

pack() { "$tapp" pack-web --identity "$state/keys/publisher.json" "$@"; }

apps=()
runtime_abi=""
if [[ -n $build ]]; then
  echo "== core from $build"
  core_wasm=$build/$(json 'console.log(require(process.argv[1]).core.path)' "$build/runtime.json")
  runtime_abi=$(json 'console.log(require(process.argv[1]).abi)' "$build/runtime.json")
  release=$(json 'console.log(require(process.argv[1]).runtime)' "$build/runtime.json")
  glue=$(mktemp -d)
  (cd "$(dirname "$core_wasm")" && find . -type f ! -name '*.wasm' -exec cp --parents {} "$glue" \;)
  pack --manifest "$repo/tools/web/web-core.tilo-app.toml" --module "$core_wasm" \
    --asset-dir "$glue" --provides "$release" -o "$state/tapps/so.tilo.runtime.web.tapp"
  rm -rf "$glue"
  publish "$state/tapps/so.tilo.runtime.web.tapp" so.tilo.runtime.web

  echo "== apps from $build"
  while IFS=$'\t' read -r id package module; do
    manifest=$repo/apps/$package/tilo-app.toml
    [[ -f $manifest ]] || { echo "no manifest $manifest for $id" >&2; exit 1; }
    extra_apps+=("$manifest=$module")
  done < <(json 'const path = require("node:path"), dir = process.argv[1];
    const index = require(path.join(dir, "apps/index.json"));
    for (const [id, file] of Object.entries(index.apps)) {
      const app = require(path.join(dir, file));
      console.log([id, app.package, path.join(dir, path.dirname(file), app.module.path)].join("\t"));
    }' "$build")
fi

for pair in "${extra_apps[@]}"; do
  manifest=${pair%%=*}
  module=${pair#*=}
  id=$(json 'const t = require("node:fs").readFileSync(process.argv[1], "utf8");
    console.log(/^\s*id\s*=\s*"([^"]+)"/m.exec(t)[1]);' "$manifest")
  pack --manifest "$manifest" --module "$module" -o "$state/tapps/$id.tapp"
  publish "$state/tapps/$id.tapp" "$id"
  apps+=("$id")
done

echo "== snapshot"
reg sign-snapshot --owner-key-file "$state/keys/owner.seed" --valid-for-seconds 86400 >/dev/null
json 'const [file, baseUrl, ownerKeyHex, runtimeAbi, ...apps] = process.argv.slice(1);
  require("node:fs").writeFileSync(file, JSON.stringify({baseUrl, ownerKeyHex, runtimeAbi: runtimeAbi || undefined, apps}, null, 2) + "\n");' \
  "$state/registry.json" "$base" "$owner_hex" "$runtime_abi" "${apps[@]}"
cat "$state/registry.json"
# The build's registry.html reads it next to itself.
[[ -z $build ]] || cp "$state/registry.json" "$build/registry.json"

if ((check)); then
  echo "== check"
  verifier=$state/verifier
  "$repo/crates/browser/web-registry-client/build-verifier.sh" "$verifier" >/dev/null
  node "$repo/crates/browser/web-registry-client/check-registry.mjs" "$state/registry.json" "$verifier" "${origins[0]}"
  exit 0
fi

cat <<EOF
== serving $base (log: $state/serve.log); Ctrl-C stops it
In the page (served from ${origins[*]}):
  import init, * as bindings from './registry_verifier.js';   // build-verifier.sh output
  import {createVerifier} from './registry-verifier.mjs';
  import {createRegistryClient} from './registry-client.mjs';
  await init();
  const registry = createRegistryClient({baseUrl: '$base', ownerKeyHex: '$owner_hex',
    runtimeAbi: '$runtime_abi', verifier: createVerifier(bindings)});
  setAppResolver(registry.resolveApp);
EOF
wait "$server"
