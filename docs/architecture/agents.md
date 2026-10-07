# Agents are collaborators, not chat features

An agent is a non-human principal that joins the same workspace as a person,
uses the same typed resources and actions, and remains visibly non-human in
every surface. Chat may create or discuss work, but it is not the management
surface and is never required to use the system.

## Product surfaces

The live collaborator popover answers **who is here now**. People and agents
can appear together there, but every agent uses a non-human identity mark and
an `AGENT` label. Selecting a participant opens its durable profile.

Workspace Settings answers **who can act here**. It keeps People and Agents as
separate destinations. An agent row remains available while the runtime is
offline and shows its admission boundary, scoped grant count, presence and
last real activity. Removing it revokes membership and every scoped grant.

Workspace Activity gains a Work destination for durable tasks. Tasks are not
messages: they have an assignee, linked resources, typed action trace, state,
timestamps and resulting changes. A person can create and dispatch a task with
parameter forms derived from action descriptors and no model at all.

## Interaction contract

An agent should be approachable like another collaborator without pretending
to be a person. Keep its name, collaborator colour and localized Agent label
visible together. Its presence describes real activity, not an animation that
claims work happened. A semantic action can highlight its actual resource's
UI element; it must not fabricate pointer events or suggest that an action
operates through screen coordinates. Only genuine pointer control publishes
pointer presence.

The conversation is ordinary Chat, not a separate agent terminal. Its compact
detachable presentation uses the same shared floating-content primitive as
navigation and calls, rather than adding overlapping desktop windows or an
app-owned title bar. Detach and Return change presentation, not conversation,
authority or editor identity: the private draft, caret, selection, undo stack,
IME composition and focus remain with the same editor. Another participant
must never receive this private editor state.

Activating the agent's resource-anchored presence opens a small composer beside
that anchor. Freeze the anchor while editing and clamp or flip the composer
inside the viewport. Moving activity must not chase the person's pointer or
move the field under their caret. Expired presence retires the marker's
navigation target, not a conversation the person already started. The open
composer retains its private draft and stable Agent target; each submission
still requires fresh action authority and, for Steer, the selected current run.
Lost permission disables submission without discarding the draft or stealing
focus. An ended action must not interrupt typing or pretend the Agent is still
performing that action.

The composer has three explicit intents, with a private draft for each:

- **Message** submits to the canonical conversation. A conversation receipt
  means Chat accepted it, not that the agent read or answered it.
- **Queue** creates separate pending work. A queue receipt is historical
  acceptance, not evidence that work is running or remains unstarted.
- **Steer** addresses one selected current task and its exact run epoch and
  revision. Saving direction does not mean the agent applied it. Multiple
  current tasks require an explicit choice, never an arbitrary first task.

Keep the full conversation reachable from the composer. Current intent and
result belong next to the submitted work, not in a transient toast alone.
Receipts that arrive while the composer is closed survive reopening; a receipt
for an earlier submission never clears a subsequently edited draft or claims
to describe that new draft. Switching agents, intent or presentation must not
drop an in-flight invocation. An unknown outcome retains the original UUID and
disables duplicate submission; observing its receipt is not a fresh retry.

The task inspector is the place for the full owner-backed instruction, linked
resources, pending work and attributed results. Public presence carries a
short intention and actual authenticated phase, not private chain-of-thought.
Distinguish a starting lease from agent acceptance, planning or execution.
Display Saved versus Applied direction and Stop requested versus Cancelled
separately. Live status requires a fresh observation of the same task and run;
an old receipt cannot describe a replacement run. Amending or cancelling
pending work uses an owner revision fence against concurrent admission.

### Native Message boundary

Adding an agent to a room grants neither the room's Chat history nor general
Chat write authority. Explicitly addressed messages need a durable owner-backed
input receipt minted after the canonical message commit. It binds the admitted
invocation, original message and author, target agent, room and canonical
record. Delivery contains only the explicitly submitted bounded text. Reply
authority is limited to that original parent and authenticated Agent author.
Acceptance and reply revalidate the actual managed process and current launch
custody; a retired or replaced process cannot inherit a prior receipt simply
by sharing a display name. A stored custody projection is correlation, never
authority. Whole-document replicas or a fixture reply do not prove this path.

### Acceptance gates

These are requirements, not a declaration that all gates already pass:

- Inspect full-size before/after compositions, including actual resource
  presence and composer together, at normal and compact sizes in light, dark
  and high-contrast enlarged text. Reject clipped controls, missing Agent
  labels, moving anchors and receipts that misstate the task's phase.
- Exercise physical detach, typing, selection, undo, IME input and Return on
  the same native editor. Check fresh primary and detached frames, not a
  retained buffer's presence alone. Exercise keyboard and accessibility paths
  through the same controls and public actions.
- Prove a signed canonical submission reaches the actual managed child,
  produces a gateway request and commits an attributed canonical reply that
  is presented in Chat. A controlled provider proves the pipeline, not the
  quality or availability of a configured external model.
- Prove permission downgrade, retirement, process replacement, cross-agent
  isolation, duplicate delivery, missing receipts and late receipts without
  widening permissions or repeating effects. Timer-only polling must stop at
  a bounded response window and must not force idle repaints.

## Access model

Human roles are inherited base authority. Agents deliberately receive no
human role: `Workspace::actor_for_agent` produces a typed agent principal with
no role, and every usable resource/action is an explicit capability.

The UI therefore must not call an agent's room-admission verb a role. Future
presets are capability bundles, expanded into ordinary `CapOverride` records:

- **Observer** — inspect selected resources, no mutations.
- **Contributor** — comment or edit only selected apps/resources.
- **Operator** — broader selected actions/files, excluding administration,
  devices and unrestricted network by default.
- **Custom** — the exact capability editor.

Presets are conveniences, not a second policy store. The UI derives the preset
name when the live capability set exactly matches a bundle and shows `Custom`
otherwise. Pause must be an enforced restriction, never a display-only state.

## Provider and model architecture

Tilo owns identity, credentials, grants, confirmation, audit and runtime
lifecycle. Provider translation is delegated to mature components:

```text
Agent profile / Task Center
           |
           v
Tilo model gateway (per-user, secrets stay here)
           |
           +-- LiteLLM Proxy -> OpenAI, Anthropic, compatible cloud APIs
           +-- vLLM OpenAI-compatible server -> local model
           +-- compatible endpoint -> remote vLLM, LM Studio, Ollama, etc.
           |
           v
Agent runtime -> Tilo action/resource gateway -> collaboration broker -> app
```

[LiteLLM](https://docs.litellm.ai/) is a provider-normalization layer only; it
does not decide Tilo permissions or own downloads. Provider profiles store a
stable id, label, endpoint kind, base URL, model alias, enabled state and a
reference to a secret in the accounts vault. Agents connect only to the Tilo
gateway; provider credentials never enter their sandbox.

Local installation uses the [Hugging Face Hub download/cache
workflow](https://huggingface.co/docs/huggingface_hub/guides/download), pinned
to an immutable revision, and starts [vLLM's OpenAI-compatible
server](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/)
against that resolved snapshot. Tilo presents license, size, revision,
download/load progress and VRAM state, then supervises the process behind a
loopback or network-namespace boundary. Downloading is an explicit management
operation, never a side effect of an agent request.

## Agent experience and MCP

Foreign Linux agent boxes (no desktop) use the MCP-beside-local-relay path in
[`agent-host.md`](agent-host.md) rather than embedding a cloud API or copying
the user root secret.

`crates/collab/actions` remains the single action schema. The gateway projects
each permission-filtered `ActionDescriptor` to MCP with its existing JSON
parameters, description, risk/confirmation contract and canonical address.
Calls return through the broker invocation path; MCP never calls app code
directly.

Small models should retain the bounded discovery sequence already used by the
agent app: list applications, list operations, describe one operation, then
invoke a descriptor revealed in the current plan. Tilo can expose these as MCP
tools alongside directly registered high-value actions.

Resources need the same treatment. The broker should publish inert typed
resource descriptors—including resources with no actions—and project them to
stable `tilo://workspace/...` MCP resource addresses. Hard-coded facts in the
agent app can then disappear as apps adopt the shared contract.

## Delivery order

1. **Identity and access truth** — preserve agent kind end to end; durable
   Agents page; explicit agent permission subjects; immediate revocation.
2. **Provider-neutral model protocol** — retain Anthropic as one adapter, add
   OpenAI-compatible tool calls, provider profiles and vault-backed secrets.
3. **Task Center** — manual typed tasks first, then optional model-backed
   planning; cancellation/retry only where the action contract permits it.
4. **Local runtime** — explicit HF snapshot lifecycle, hardware fit checks and
   supervised vLLM status.
5. **Resource/MCP projection** — publish permission-filtered resources beside
   actions and remove per-app fact tables.

Each slice must prove that revocation changes both catalogue visibility and
invocation authority. A management UI cannot claim a grant is effective until
the broker revalidates the typed agent principal against the current workspace.
