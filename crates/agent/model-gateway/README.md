# Model gateway

The native service core is `service::Gateway`. It accepts a host-authenticated
`caller::Session`, not an app ID from an RPC request. The Unix launcher/FD
adapter and command line remain available as `tilo-model-gateway`.

`http_service::HttpService` is a small HTTP/1 adapter with an injected
`SessionAuthorizer`. A relay or another authenticated host can mount `handle`
or pass an accepted TLS stream to `serve_io`; no relay/node runtime is required.
The authorizer must validate session audience and revocation, and origin/CSRF
when using cookies. Settings identity alone is insufficient for credential
status/set/remove: the session must explicitly grant credential management.
There is no credential-read operation.

The service is single-user: caller roles are capabilities, not tenant identities.
Run separate processes with isolated configuration, storage and credential custody
for different users, and route each authenticated session only to its user's
instance. Do not put grants for unrelated users in one session directory.

## Build features

Defaults are empty: native and browser UI consumers get only the protocol and
client transports, without provider credentials, inference or C++ model runtimes.
Do not enable server features on standalone UI builds.

| Feature | Deployment |
| --- | --- |
| `server` | Hosted providers and the light HTTP/Unix service, without local inference. |
| `local-models` | Full native gateway with installed segmentation models; includes `server`. |
| `host-backend` | Trusted-host model backend; includes `server`, not local inference. |

Both service binaries require `server`. Desktop packaging selects `local-models`;
HTTP relays need only `server`. `host-backend` is for trusted hosts, never apps.

```sh
# Desktop gateway (normally socket-activated by the user service).
cargo run -p tilo-model-gateway --features local-models --bin tilo-model-gateway -- --help
# Trusted-host adapter and hosted provider service, without local inference.
cargo check -p tilo-model-gateway --features host-backend --all-targets
```

## Standalone service

`tilo-model-http` binds loopback and uses `SessionDirectory`, an adapter for
host-issued expiring bearer sessions. It does not create identities or accept
an app-ID header/JSON override. The trusted host must generate a random 32-byte
hex bearer and privately deliver it to its authenticated browser session.

Create an owner-only session directory. Each grant is an owner-only regular
file named with the lowercase SHA-256 hex digest of the **ASCII bearer**:

```json
{"caller":"agent","expires_unix_ms":1790000000000}
```

Use an actual future expiry. Allowed callers are `agent`, `chat`, `photos`,
`settings`. Chat receives only tool-disabled text completion without tool
history; it cannot acquire the other callers' operations.
For a separately authorized Settings management session only, add
`"manage_credentials":true`. Never put provider keys in these files. Deleting
or changing a grant revokes in-flight calls within 100 ms; expired sessions
cannot start requests. Non-private directories/files and symlinks are refused.
Session issuance belongs to the authenticated host, not this service.

```sh
cargo run -p tilo-model-gateway --features server --bin tilo-model-http -- \
  --session-directory "$XDG_RUNTIME_DIR/model-sessions"
```

Terminate public HTTPS at a trusted proxy and route same-origin `/model/rpc`
to the loopback listener (default `127.0.0.1:8765`). Do not expose cleartext HTTP
publicly. No permissive CORS policy is installed. Configure providers on the
server, using the same settings and credential store as the native gateway.

## Browser client

`transport::FetchTransport` implements the async, transport-neutral
`Transport` (`Request` → intermediate `Response` events → final `Response`).
It uses `tilo-net`, which uses browser fetch on the web; it requires an HTTPS
`/model/rpc` URL and the host session bearer, never a provider API key. Install
it in the host's model backend rather than a process-global singleton.
`transport::ModelClient<T>` adapts any transport to the portable
`agent_provider_config::protocol::ModelBackend` for one selected profile/model.

HTTP responses are no-store NDJSON. Requests/individual events are capped at
2 MiB, intermediate events have a bounded eight-frame queue and event count,
and active calls/connections and deadlines are bounded. Returning false from
the event callback or dropping the call future cancels fetch; disconnect and
session revocation drop server-side provider work. A slow reader cannot retain
a call indefinitely. Already-started blocking credential-store mutations are
not rolled back by disconnect/revocation.

## Production path

Native launcher-bound clients and browser host-injected clients share this
service core. Chat's standalone bootstrap installs `Models::from_launcher()`;
component mounts capture `HostContext::models()` independently. Agent consumes
the portable stream and publishes negotiated, authenticated task presentation,
so Chat readers need no provider connection. The explicit Chat assistant controls
use a separate prose-only capability; Agent's action-capable loop uses the
permission-checked resource/action broker, not a tool dispatcher in Chat.

## Verification

A default-feature test run exercises clients only, not the production server.
Run the feature configurations explicitly:

```sh
cargo test --locked -p tilo-model-gateway --no-default-features
cargo test --locked -p tilo-model-gateway --features server
cargo test --locked -p tilo-model-gateway --all-features
cargo check --locked -p tilo-model-gateway --all-features --tests
```

Native CI selects `local-models,host-backend` for the gateway package itself,
including server and segmentation tests; it does not enable those features for
client-dependent apps. Focused native test requests retain exact-test listing
and execution guards: a missing or ignored test fails rather than counting as
server coverage. These tests use local fixtures, not live provider credentials.

## Streaming and credentials

Rig 0.44 owns provider JSON, SSE event decoding, reasoning normalization and
argument assembly. Its OpenAI chat decoder rejects EOF without a finish reason,
but allows EOF after an explicit finish (some compatible providers omit
`[DONE]`). Unknown valid JSON events are ignored; malformed JSON fails the turn.
The gateway additionally rejects an undelimited SSE tail, which Rig's framer
otherwise discards. It does not infer success from a connection closing.

`complete_streaming` (native or `transport::ModelClient`) emits text and
provider-exposed reasoning deltas. Reasoning can be raw provider-exposed tokens
or provider-generated summaries; the presentation must identify the actual
channel rather than claim every provider returns a summary. No client invents
or exposes its own hidden chain of thought as a substitute. Apply these to the portable `StreamState`
to retain partial presentation after a failed/cancelled turn. Finished responses
keep output-limit and refusal reasons explicit. Tool deltas are inert;
completed legacy tool events are withheld until the entire turn finishes with
valid arguments and unique call IDs. Failed, truncated and refused turns never
authorize calls. Bounded retries are permitted only before response headers
when the transport proves no request was sent, or when the provider explicitly
refuses an unprocessed request. Unknown outcomes and interrupted generation are
not automatically replayed.

Provider keys use `accounts::ServiceCredentials`, bound to the model-gateway
service, provider-key purpose and exact canonical endpoint origin. Storage and
durable-write failures are reported, not treated as missing credentials or
successful writes. Legacy unbound keys are not silently adopted from the
current editable endpoint. Re-enter the key, or use the accounts migration API
with an independently known original origin. Remove a bound key before changing
its origin; if already changed, restore the original endpoint to remove it.
Provider diagnostics use fixed actionable advice, never provider messages,
arbitrary identifiers, headers or credential bytes.
