# Hosted model gateway

`tilo-model-gateway` is a socket-activated per-user systemd service. Its private
socket exists for the launcher at session start, but the process and network
stack are not loaded until Settings or Agent actually connects. It owns outbound calls to
the explicitly supported OpenAI-compatible and Anthropic endpoints, and resolves API keys
from the Tilo encrypted account vault. Sandboxed clients receive only an
authenticated, launcher-passed `model-gateway` service FD in
`TILO_SVC_MODEL-GATEWAY_FD`. The launcher uppercases the service name and
preserves its hyphen. Clients never receive the key or direct provider network
access.

The desktop package builds the gateway with `local-models`, which includes
`server` and the installed segmentation-model runtime. A hosted HTTP relay
uses `server` alone. Apps use the empty default feature set: native standalone
UI builds do not embed provider credentials, inference or C++ model runtimes.
The `host-backend` feature is reserved for trusted hosts, not sandboxed apps.

The implementation is intentionally a narrow relay, rather than claiming a
general LiteLLM-compatible provider surface: only the two existing provider
protocols and the agent planning request are supported. This keeps the
credential and outbound surface auditable. A future pinned LiteLLM sidecar
can implement the same gateway boundary without exposing its admin API to
apps. The
gateway validates its current non-secret provider configuration before each
call. API keys are supplied by Settings through the same FD, stored in the
vault, and are never written to `settings.json`, CRDT state, or logs.
