# Host-provided assistant

Chat's component definition captures `HostContext::models()` when mounted. The
capability is local to that mount: opening another Chat does not replace its
backend. No provider credentials or ambient model client are read by Chat.

Sending a message remains person-to-person. **Ask assistant** explicitly asks the
injected backend about the latest nonempty message authored by the local person
in that conversation. The channel resource exports the device-only
`model-control` effect with an `ask`, `retry`, `continue` or `stop` command.
The handler checks which commands are currently applicable.
The painted shared buttons expose the same controls to accessibility.

A start acknowledgement contains `started: true` and a new `reply` id. It means
that this mount installed a local inference future, not that the provider has
accepted or completed inference. A stop acknowledgement contains `stopped: true`
and the reply id after the local future has been dropped. It does not promise
remote billing cancellation. Unavailable or repeated controls fail rather than
reporting fabricated success.

Each attempt writes ordered text and provider-exposed reasoning into the existing
`AgentStream` presentation. Partial output, failure detail, and terminal outcome
remain on that reply. Retry and prose continuation allocate new replies and run
ids. Continuation includes retained answer text, not reasoning or partial tool
arguments. All requests disable tools, and no streamed or final tool calls are
dispatched. Existing committed action traces are neither cleared nor replayed.
Recovery is local to the live mount; reopening retains the document's output and
status, not an executable inference future.

## Streaming backend contract

`ModelBackend::stream(&ModelRequest, StreamSink) -> CompletionFuture` is a local,
non-`Send` future. A backend can override it to emit presentation events while
pending. The default implementation adapts `complete()` by emitting final text
and reasoning. The returned response is authoritative; returning an error is not
successful EOF. Dropping the future cancels the local subscription. The SDK's
`Models::stream` forwards only to the captured backend.

## Native and browser integration

The standalone native bootstrap in `main.rs` acquires `Models::from_launcher()`
and installs it on this Chat instance. Without a launcher capability it remains
unavailable; it does not read a provider key or borrow an agent identity.
The gateway authorizes `so.tilo.chat` separately: text completion with tools
disabled and no tool history, not the Agent, Photos or Settings operations.
Mounted component hosts inject their own `HostContext::models()` capability.
Native launcher transport and the browser host's authenticated HTTP transport
converge on the same gateway backend and provider protocol.

The gateway uses Rig for provider translation and streaming, and the common
accounts credential broker for service-, purpose- and origin-bound provider
keys. A light HTTP host can expose the gateway without running a relay/node.
See [`model-gateway`](../../crates/agent/model-gateway/README.md) for session
authentication, revocation and deployment requirements.

Provider-exposed reasoning is retained separately from answer text and labelled
as provider reasoning, whether it consists of tokens or a provider summary.
Chat never fabricates the assistant's hidden chain of thought. The assistant
controls here are prose-only; action-capable agents retain their separate
permission-checked tool execution path and durable operation trace.
