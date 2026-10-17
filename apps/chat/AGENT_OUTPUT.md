# Agent output in Chat

Chat receives model output through two authenticated paths, without giving
readers a provider connection:

- Task conversations receive negotiated `TaskPresentation` updates through Task
  Center. The host validates the agent, run epoch and sequence, authors receipt
  timestamps, and retains incremental CRDT text separately from task control.
  Chat projects one stable reply per model turn, including exposed reasoning and
  terminal outcome. Older hosts retain readable progress updates.
- The host-provided prose assistant writes the room's replicated typed Chat
  record through the `stream-output` action described below.

Neither presentation path grants authority to execute tools.

The channel's `stream-output` action takes `channel`, a fresh `reply` id, an
optional display `name` (never authority) and an `event` JSON value. The first accepted event creates an empty reply attributed
to the invoker; subsequent events require that same author. Events carry a
stable `run`, a one-based contiguous `sequence`, and one of:

```json
{"run":"unique-attempt-id","sequence":1,"event":{"type":"text_delta","text":"Hello"}}
```

- `text_delta { text }` appends to the answer without creating edit history.
- `reasoning_delta { text }` appends **only provider-exposed reasoning** to a
  separate, initially collapsed section. The agent's existing decision explanation
  (`AgentWork.reasoning`) is not relabelled as model reasoning.
- `progress { label }` records actual working/tool progress, never timer-generated
  activity. Actual dispatched operations remain in `AgentWork.trace`.
- `finished { outcome, detail }` records `complete`, `truncated`, `refused`,
  `cancelled`, or `failed`. Supply a safe, actionable error detail, without
  credentials or private request headers. Partial output survives every outcome.

The stream producer must publish transport errors and premature EOF as `failed`,
not `complete`, and publish `cancelled` only after cancellation is observed.
Coalesce small deltas before publication. Duplicate, out-of-order, oversized,
wrong-run and post-terminal events are refused. Text is bounded to 64 KiB and
provider reasoning to 256 KiB per reply. The single stream producer must publish
all its events in order through the same document replica; independent writers
are not supported for a single run.

The channel action refuses updates to another author's reply. Reading a message
returns the answer and stream metadata under `work.stream`. The message's
`show-agent-reasoning` view effect takes `expanded`; the shared button supports
pointer, keyboard and accessibility activation in both the timeline and thread
view. Reasoning is stored as native CRDT text so appending does not rewrite
earlier reasoning into document history.

Retries require a new reply and run id. Presentation replay never dispatches a
tool. Action-capable task conversations keep Task Center's authenticated queue,
direction and stop controls; their receipts distinguish acceptance from execution.
The separately injected, prose-only assistant has lifecycle controls described in
[HOST_MODELS.md](HOST_MODELS.md); those do not control a collaborating Agent's
run. Review completed and unconfirmed tool outcomes before directing a task to
continue. Prose continuation never resubmits an action-bearing prompt.

A producer should coalesce portable gateway events and submit them through
`Chat::STREAM_OUTPUT.with(chat::StreamOutput { channel, reply, name, event })`.
Reconcile the final provider response before recording the run outcome.
The separate `record-work` action still displays public progress and operation
outcomes, but cannot supply provider token deltas by itself and does not erase
an existing provider stream.

The reasoning section is labelled **Reasoning shared by the provider**. It may
contain provider-exposed reasoning tokens or a provider-generated summary; it
is not necessarily a summary. Chat does not suppress that channel merely because
it contains raw provider reasoning, invent reasoning for providers that expose
none, or substitute the assistant's own hidden chain of thought.

Focused tests: `cargo test --locked -p chat --lib agent_stream`.
Offscreen captures use Chat's own `--shots` facility, not a live desktop:

```sh
cargo build --locked -p chat --bin chat
mkdir -p target/chat-review
for theme in light dark; do
  for scenario in agent-streaming agent-failed agent-reasoning; do
    TILO_SHOTS_THEME=$theme target/debug/chat \
      --shots target/chat-review/$scenario-$theme.png --scenario "$scenario"
  done
done
```

Add `--viewport 760x900` for a narrow capture. Fixtures are ephemeral and do
not send inference requests or execute tools. Successful PNG creation alone is
not verification: inspect that the requested agent state is actually visible.
