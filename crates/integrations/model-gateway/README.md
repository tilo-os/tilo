# Model gateway

`tilo-model-gateway` is the service apps reach over the descriptor their
launcher hands them. The launcher's handoff names the app; nothing in a
request's JSON can change who is asking. Programs that speak a provider's own
HTTP dialect (Messages, chat completions) ask this computer's one model
endpoint in the daemon instead, which answers from the model the workspace
chose.

The service is single-user: caller roles are capabilities, not tenant identities.

## Build features

Defaults are empty: native and browser UI consumers get only the protocol and
client transports, without provider credentials, inference or C++ model runtimes.
Do not enable server features on standalone UI builds.

| Feature | Deployment |
| --- | --- |
| `server` | Hosted providers and the Unix service, without local inference. |
| `local-models` | Full native gateway with installed segmentation models; includes `server`. |
| `host-backend` | Trusted-host model backend; includes `server`, not local inference. |

The service binary requires `server`. Desktop packaging selects `local-models`.
`host-backend` is for trusted hosts, never apps.

```sh
# Desktop gateway (normally socket-activated by the user service).
cargo run -p tilo-model-gateway --features local-models --bin tilo-model-gateway -- --help
# Trusted-host adapter and hosted provider service, without local inference.
cargo check -p tilo-model-gateway --features host-backend --all-targets
```

## Clients

`transport::FdTransport` implements the async, transport-neutral `Transport`
(`Request` → intermediate `Response` events → final `Response`) over the
launcher's descriptor. `transport::ModelClient<T>` adapts any transport to the
portable `agent_provider_config::protocol::ModelBackend` for one selected
profile/model.

A page keeps no provider key and asks no provider itself: its windows ask the
person's own model on their workspace server (the `tilo.models` `ask` action),
which the server answers with the key the person's vault keeps there, as it
answers their hosted agents.

## Production path

Chat's standalone bootstrap installs `Models::from_launcher()`;
component mounts capture `ProcessContext::models()` independently. Agent consumes
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

`agent_provider_config::wire` is the one codec for provider requests, answers
and their event streams, shared with the browser page's model service: it
writes the request body (with every profile setting applied), decodes whole and
streamed answers, and checks a turn before any call in it may run. A chat
stream needs a finish reason, a Messages stream its `message_stop`, a Responses
stream its completed answer; an event without its closing blank line counts as
cut. Unknown valid JSON events are ignored; malformed JSON fails the turn. The
gateway does not infer success from a connection closing.

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

Signed-in providers carry no key: the gateway asks account credentials for a
current access token for the profile's own grant (`sign_in_grant_id`),
refreshed there when due, and keeps it no longer than its expiry. A Google
sign-in adds the billing project; a ChatGPT sign-in speaks Responses to the
ChatGPT backend with the account its token names; a Copilot sign-in names the
editor GitHub's API requires, its token minted from the GitHub grant.
`ListModels` lists a provider's models with the same credential, for Settings'
model picker and connection test; it needs the credential-management grant
and answers only model ids, names and context sizes.

Provider keys use `accounts::ServiceCredentials`, bound to the model-gateway
service, provider-key purpose and exact canonical endpoint origin. Storage and
durable-write failures are reported, not treated as missing credentials or
successful writes. A key from before origin binding is not read or adopted from
the current editable endpoint: re-enter the key. Remove a bound key before
changing its origin; if already changed, restore the endpoint it was saved with
to remove it.
Provider diagnostics use fixed actionable advice, never provider messages,
arbitrary identifiers, headers or credential bytes.
