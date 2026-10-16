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

Human roles are inherited base authority. An agent is not a member with a role
of its own: `Workspace::actor_for_agent` produces a typed agent principal, and
it acts for one person. Its room authority is the least of that person's role,
what its room admission grants it, and Editor
(`tilo_action_policy::roles::agent_policy::role`, capped at `ROLE_CAP`). Its
delegation's grants then bound it further, so an agent never governs a room and
never does more than its person.

The UI therefore must not call an agent's room-admission verb a role. Future
presets are capability bundles, expanded into ordinary `CapOverride` records:

- **Observer** — inspect selected resources, no mutations.
- **Contributor** — comment or edit only selected apps/resources.
- **Operator** — broader selected actions/files, excluding administration,
  devices and unrestricted network by default.
- **Custom** — the exact capability editor.

Agents share their delegation with bots ([bots.md](bots.md)). Both act from a
device key of their own under a root-signed, expiring `Delegation` whose
`delegate` names an agent or a bot installation, and both are rows of the
room's governed delegate list. The difference is who decides the steps: an
agent plans at run time within its grants, while a bot runs a fixed program
whose grant was derived from it and reviewed at install. A bot is therefore
not an agent preset, and agent presets never apply to bots. Bots are not capped
at Editor, because their grant is fixed and reviewed. They still never govern
the room ([bots.md](bots.md) §3).

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
