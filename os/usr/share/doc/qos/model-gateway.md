# Hosted model gateway

`qos-model-gateway` is a per-user systemd service. It owns outbound calls to
the explicitly supported OpenAI-compatible and Anthropic endpoints, and resolves API keys
from the qOS encrypted account vault. Sandboxed clients receive only an
authenticated, launcher-passed `model-gateway` service FD; they never receive
the key or direct provider network access.

The implementation is intentionally a narrow relay, rather than claiming a
general LiteLLM-compatible provider surface: only the two existing provider
protocols and the agent planning request are supported. This keeps the
credential and outbound surface auditable. A future pinned LiteLLM sidecar
can implement the same gateway boundary without exposing its admin API to
apps. The
gateway validates its current non-secret provider configuration before each
call. API keys are supplied by Settings through the same FD, stored in the
vault, and are never written to `settings.json`, CRDT state, or logs.
