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

### Tasks, assignments and presets

No feature names a provider or a model. It names a **task**: a typed,
registered capability consumer in `agent_provider_config::tasks` with a stable
id, the consumers allowed to ask for it (an app id, or the desktop session),
the capabilities a model must declare (tools, vision, face recognition, trace
review, a minimum context window) and a privacy class:

| Class | Where it may run | Tasks |
| --- | --- | --- |
| Open | Any configured provider | `agent.planning` |
| Sensitive | This device, unless the person explicitly allows a provider | `feedback.review` |
| Device only | Only the model manager's supervised runtime | `photos.vision`, `photos.faces` |

The registry is compiled into the gateway, so a sandboxed app cannot declare
its own privacy class. Adding a feature's task is adding a `TaskSpec` there.
Tasks whose request the caller writes use the `Plan` verb; tasks whose prompt
the gateway owns use a fixed verb (`AnalyzeImage`, `ClusterFaces`,
`ReviewTrace`).

The gateway resolves `(consumer, task, agent)` with `assign::resolve`, most
specific first: an assignment for this agent, then for the task, then the
selected preset, or with no preset the task's own recommendation (Open tasks
on the default provider, the others on this device). An explicit assignment
that cannot be honoured is an error, never a fall-through, and every layer is
checked against the task's capabilities and privacy class; only the
supervised runtime counts as this device. Typed model options (fast mode,
reasoning effort) are declared per model and written by the gateway in each
provider's own dialect.

Presets (Private, Balanced, Best quality) are derived on every resolution from
the device's hardware snapshot and the configured providers, not stored as
assignments, so they adapt when either changes. Balanced keeps Open tasks
local only on a card with at least 15 GiB; a card waiting for a restart holds
the preset instead of degrading it. No preset grants a Sensitive task a
provider.

Scope, decided in `agent_provider_config::assign`: provider profiles and
assignments, per-agent ones included, are personal (`agent_providers.profiles`
and `.assignments`) and follow the person to each device. A per-agent choice
is personal because the agent runs through the person's gateway and spends
the person's keys; a workspace admits an agent but does not choose which of a
person's providers it bills. Keys and account tokens are secret, in each
device's vault. The model manager's runtime profile and hardware facts are
device-local (`agent_providers.local`); an assignment names a local model by
catalogue id, so it resolves on whichever device runs it.

Settings' AI pane is the one hub for all of this, and every control in it is
an action of its resource tree, so agents and `tilo-control` configure it the
same way; keys, removals and downloads need confirmation. Azure OpenAI signs
in with Microsoft Entra ID through `accounts::oauth`, the one OAuth
implementation Mail also uses.

Local installation uses the [Hugging Face Hub download/cache
workflow](https://huggingface.co/docs/huggingface_hub/guides/download), pinned
to an immutable revision, and starts [vLLM's OpenAI-compatible
server](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/)
against that resolved snapshot. Tilo presents license, size, revision,
download/load progress and VRAM state, then supervises the process in a
container without network, reachable only through a private Unix socket. Downloading is an explicit management
operation, never a side effect of an agent request.

## Agent experience and MCP

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
