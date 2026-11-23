#!/usr/bin/env bash
# The developer docs' hosted-Browser example (docs/developers/one-workspace-per-customer.md,
# example `hosted-browser`), run against a local tilo.sh stack with its VM host in it: a
# workspace's Browser opens in the workspace's own VM and an agent's key drives the page through
# the SDKs' generic action call. The stack is the one os/containers/tilo-sh/stack.sh brings up,
# from the programs and images t hands it (TILO_SH_*); the page is the visa test form, served
# where a VM may reach it — the server endpoint's address as the VM dials it.
#
# The examples come from tools/docs/doc_examples.py, the runner the docs-examples check runs the
# book's other examples with: `--host stack` selects exactly the blocks that name `host=stack`,
# and leaves it to this check to have exported TILO_API_URL and TILO_API_KEY.
#
# usage: test-docs-browser.sh WORK
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
work=${1:?usage: test-docs-browser.sh WORK}
state=${TILO_SH_STATE:?TILO_SH_STATE names where the stack keeps its state}
base=${TILO_SH_PORT_BASE:-24100}
mkdir -p "$work"
find "$work" -mindepth 1 -delete

# The Python SDK and TypeScript SDK as the docs-examples check runs them: the ci-sdk image's
# uv environment, bun cache and locked packages, copied because a job's image is read-only.
sdk=$work/sdk
mkdir -p "$sdk"
export UV_OFFLINE=1 UV_PYTHON_DOWNLOADS=never
export UV_CACHE_DIR=$sdk/uv-cache UV_PROJECT_ENVIRONMENT=$sdk/python-venv
cp -a /opt/tilo-sdk/uv-cache "$UV_CACHE_DIR"
cp -a /opt/tilo-sdk/python-venv "$UV_PROJECT_ENVIRONMENT"
export BUN_INSTALL_CACHE_DIR=/opt/tilo-sdk/bun-cache

# The stack, with its VM host: the workspace's Browser runs in its own VM.
step() { printf '\n== %s\n' "$*"; }
step 'bring the stack up, its VM host included'
bash "$here/stack.sh" up --fresh
trap 'bash "$here/stack.sh" down >&2 || true' EXIT

# The stack's addresses and helpers.
# shellcheck source=stack.sh
. "$here/stack.sh"

# The page a VM may reach: the server endpoint's own address and port, which pasta maps to this
# machine's loopback (check.sh serves its form there too).
step 'serve the visa test page where a VM reaches it'
form=$work/form
mkdir -p "$form"
cp "$repo/crates/engines/chromium-host/test-pages/visa-form.html" "$form/visa-form.html"
form_port=$iroh_port
python3 -m http.server --bind 127.0.0.1 --directory "$form" "$form_port" > "$work/form.log" 2>&1 &
form_pid=$!
for _ in $(seq 100); do
  curl -fsS --max-time 1 "http://127.0.0.1:$form_port/visa-form.html" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -fsS --max-time 2 "http://127.0.0.1:$form_port/visa-form.html" >/dev/null \
  || { cat "$work/form.log" >&2; echo "the page server did not answer on 127.0.0.1:$form_port" >&2; exit 1; }

# The stack's own person approves a key for this check (it reaches all their workspaces), and
# the example reaches the API the way the published one reaches tilo.sh.
eval "$(bash "$here/stack.sh" env)"
eval "$(bash "$here/stack.sh" owner --here)"
[[ -n ${TILO_API_URL:-} && -n ${TILO_API_KEY:-} ]] \
  || { echo 'the stack named no API server or key (stack.sh env, owner --here)' >&2; exit 1; }
export TILO_BROWSER_URL="http://10.0.2.4:$form_port/visa-form.html"

step 'run the hosted-browser example'
cd "$repo"
python3 -I tools/docs/doc_examples.py run \
  --host stack --only hosted-browser --timeout "${TILO_DOCS_TIMEOUT:-600}"

kill "$form_pid" 2>/dev/null || true
step 'done'
