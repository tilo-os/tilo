#!/usr/bin/env bash
# Static contract for the agent-host Compose recipe (MCP beside local relay).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compose="$root/compose.yaml"
readme="$root/README.md"
entrypoint="$root/tilo-agent-mcp"
containerfile="$root/Containerfile"
dockerignore="$root/Containerfile.dockerignore"
arch_doc="$root/../../../docs/architecture/agent-host.md"
runtime_mcp="$root/../../../docs/runtime-mcp.md"
relay_readme="$root/../tilo-relay/README.md"

fail() { echo "tilo-agent-host contract: FAIL: $*" >&2; exit 1; }
need() { grep -Fq -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }
forbid() { if grep -Fq -- "$2" "$1"; then fail "$(basename "$1") must not contain: $2"; fi; }

for path in "$compose" "$readme" "$entrypoint" "$containerfile" "$dockerignore" "$arch_doc" "$runtime_mcp" "$relay_readme"; do
  [ -f "$path" ] || fail "missing ${path#"$root/../../../"}"
done

# Three-role separation is documented.
need "$arch_doc" "Three roles"
need "$arch_doc" "Agent on an existing OS"
need "$arch_doc" "loopback"
need "$readme" "Pair home server"
need "$readme" "pair-device --headless"
need "$relay_readme" "agent host"
need "$runtime_mcp" "Agent host"

# Compose: relay + MCP sidecar, host network for LAN pair, loopback MCP.
need "$compose" "tilo-relay:"
need "$compose" "tilo-mcp:"
need "$compose" "network_mode: host"
need "$compose" "127.0.0.1:7391"
need "$compose" "./relay-identity:/var/lib/tilo:Z"
need "$compose" "./agent-profile:/var/lib/tilo-runtime/profile:Z"
need "$compose" "./mcp-secrets:/run/tilo:Z"
need "$compose" "registry.tilo.so/tilo/tilo-runtime:latest"

# Never mount or document copying the user-root private key into the recipe.
forbid "$compose" "user-identity/privkey"
forbid "$compose" "privkey.bin"
need "$readme" "Do **not** put a user-root private key"
need "$entrypoint" "pair-device"
need "$entrypoint" "--headless"
need "$entrypoint" "TILO_RUNTIME_MCP_HTTP"
need "$entrypoint" "tilo-runtime --profile"
need "$entrypoint" "127.0.0.1"

# Image builds runtime + enroll only (no compositor).
need "$containerfile" "cargo build --locked --release -p tilo-runtime -p tilo-relay"
need "$containerfile" "--bin tilo-runtime --bin tilo-enroll"
need "$containerfile" 'ENTRYPOINT ["/usr/bin/tilo-agent-mcp"]'
need "$containerfile" "tilo-agent-mcp"
forbid "$containerfile" "wayland"
forbid "$containerfile" "compositor"

for excluded in .git .forgejo target os/output; do
  grep -qxF "$excluded" "$dockerignore" || fail "Docker build context includes $excluded"
done

# Entrypoint is executable in the tree.
[ -x "$entrypoint" ] || fail "tilo-agent-mcp is not executable"

echo "tilo-agent-host contract: PASS"
