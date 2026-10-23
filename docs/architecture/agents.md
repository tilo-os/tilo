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

### Allow, Ask first, Deny

Within its admission a person tells each agent, per application, kind of
resource or single action, whether an action of each kind (reading, undoable
changes, outside effects) is allowed, asks first or is not allowed
(`tilo_action_policy::agent_answers`). These are ordinary grants on the room;
the most specific one decides. Before anyone chooses, reading and undoable
changes are allowed and outside effects ask first. An action whose contract
confirms (it sends, publishes, deletes for good) asks first whatever a broader
answer says; only "Always allow this", an answer for that one action, lifts
it.

An action that needs its person attending (`tilo_actions::ATTENDED_TAG`,
`#[effect(attended)]`: it opens a chooser for them, asks them for a credential
or a sign-in, or shows something only they may see) asks first every time: no
answer allows it, not even "Always allow this", and Deny still refuses it.
There is no other rule about who may run an action; reaching the files,
hardware or services of a device is an outside effect like any other.

Ask first pauses the agent's task run instead of refusing. The broker renders
the invocation as a draft (`tilo_actions::draft`: one field per parameter,
resource references and attachments by label, secrets withheld, a message
previewed as a message) and records it in the task's control sidecar, so the
pause outlives restarts. The person sees it on the task's issue in the Tasks
app ([tasks.md](tasks.md)), and confirms (optionally editing the draft's
fields), declines with a reason, or always allows it; the published `answer-confirmation` task
action does the same without the card. The agent's step waits and asks once more
with the same arguments, an ask the broker holds until the person answers, a
grant that decides it changes or the run stops (each announced through
`policy_change`), never on a clock; the broker runs exactly those arguments with the
person's edits, carried to dispatch by a one-use token bound to them. Other
arguments are a new question. The draft keeps no raw arguments, so secrets
never enter the room.

## Provider and model architecture

Tilo owns identity, credentials, grants, confirmation, audit and runtime
lifecycle. Provider translation is delegated to mature components:

```text
Agent profile / Tasks
           |
           v
Tilo model gateway (native launcher or light authenticated HTTP host)
           |
           +-- common accounts credential broker (keys stay server-side)
           +-- account custody (sign-in grants, refreshed server-side)
           +-- Rig provider adapters -> OpenAI, Anthropic, ChatGPT, compatible APIs
           +-- compatible endpoint -> vLLM, LM Studio, Ollama, etc.
           |
           v
Agent runtime -> Tilo action/resource gateway -> collaboration broker -> app
```

Rig is the production provider translation layer: provider JSON, SSE decoding,
reasoning normalization and tool-argument assembly. It does not decide Tilo
permissions or own downloads. LiteLLM is not a required service. Provider
profiles store endpoint/model configuration and a credential reference; the
common accounts `ServiceCredentials` broker binds provider keys to service,
purpose and canonical endpoint origin. Agents connect only to the Tilo gateway;
provider credentials never enter their sandbox. Existing host-side
OpenAI-compatible planners can opt in to [request presets](agent-model-presets.md);
these provider hints do not change credentials, context selection or authority.

Each agent plans with a model of its person's providers: the one chosen for it
in the agent's workspace settings (Model, above its apps' answers), else
Settings' default. The choice is the person's, kept with their provider
settings by agent id (`ProviderConfig::agent_models`), and is listed in
Settings' providers with the way back to the default; it grants nothing. The
desktop hands the agent its model when it starts it, as the nonsecret
profile/model/protocol selection, so a change applies from its next start.

### Setting up providers

Settings adds a provider from a known one (`agent_provider_config::presets`):
OpenAI, Anthropic, Google Gemini, Mistral, OpenRouter, Groq, xAI, DeepSeek,
Together, Fireworks, Hugging Face, Ollama and LM Studio on this device, or any
other OpenAI-compatible endpoint typed by hand. A preset fills the name,
endpoint, protocol and location; nothing records which preset made a profile.
**Test connection** asks the gateway to list the provider's models with the
stored key or sign-in (`ListModels`): the answer proves the endpoint and
credential work, and the person picks the model from it or still types any
other id.

A provider can also be set up by signing in instead of typing a key. Every
sign-in is sealed in this device's vault under the profile's own grant id,
apart from Mail's and Files' grants; the gateway mints request tokens from it
and agents never see it.

- **Google**: Vertex AI, or the Gemini API, billed to one of the person's
  Cloud projects (`x-goog-user-project`).
- **ChatGPT**: the person's OpenAI account, for their ChatGPT subscription.
  Requests go to the ChatGPT backend in the OpenAI Responses format, always
  streamed and never stored, through Rig's ChatGPT dialect. OpenAI offers this
  sign-in to no other application, so it uses the public client of OpenAI's
  Codex CLI the way Codex does; it can stop working whenever OpenAI changes
  it, Settings says so, and the scheduled `nightly-live-sign-in` check watches
  the client, its redirect and the backend.
- **GitHub Copilot**: GitHub's device sign-in with the client of GitHub's own
  Copilot extensions (unofficial in the same way); custody trades the GitHub
  token for short-lived Copilot tokens as its refresh.
- **OpenRouter**: OpenRouter's own PKCE flow for apps, which hands back an
  ordinary API key, stored like a typed one.
- **Hugging Face**: OAuth with the `inference-api` scope, once a distributor
  registers a client (`TILO_OAUTH_HUGGINGFACE_CLIENT_ID`); until then a Hugging
  Face access token is typed as the key.

A Claude subscription (claude.ai account) is not offered: Anthropic's terms
do not allow other applications to sign people in with their Claude account
or to route requests through Free, Pro or Max plan credentials. Anthropic is
set up with a Console API key.

The native launcher authenticates a caller through its capability transport.
The light HTTP adapter authenticates host-issued, expiring, revocable sessions;
it needs no relay/node runtime and never trusts a caller ID in request JSON.
Settings credential management requires an explicit grant and offers no key-read
operation. See [the gateway deployment contract](../../crates/agent/model-gateway/README.md).

Chat's native bootstrap installs launcher-backed models, while component hosts
inject a mount-local model capability. Chat's assistant is explicitly prose-only;
its caller cannot request tools or tool history. The action-capable Agent loop
retains bounded discovery, confirmation and broker authorization. Provider tool
deltas remain inert until a successful complete turn validates arguments and
unique call IDs; failed, truncated or refused turns do not authorize calls.

A model whose descriptor declares `vision` is shown the pictures its invokes
answered with: a Canvas look, a PDF page (`look-page`), a photo. After the
turn's results the loop adds them as images (PNG, JPEG, GIF or WebP; at most
four a turn and 4 MiB each), read from the device content the agent was handed.
Any other model sees only their content addresses.

The Agent producer streams ordered answer and provider-reasoning events through
the replicated Chat document, retaining partial output and terminal outcomes.
The reasoning UI identifies **Reasoning shared by the provider** honestly:
provider-exposed reasoning tokens need not be summaries and are not suppressed
merely for being raw reasoning. No reasoning is fabricated when a provider
exposes none, and the assistant's own hidden chain of thought is never substituted.

Local models come from two sources: the signed catalog's curated
recommendation, and any model a person picks on Hugging Face (search, an id or
link, their own fine-tunes) or adds as a GGUF file from the device. The
per-user model manager pins a Hub pick to the commit it was shown at, downloads
each file resumably and verifies it against the hash the Hub publishes (the
LFS SHA-256 of weights, the git blob id of small config files) before it enters
the device's content-addressed store. Settings shows every variant's files,
size, fit for the probed hardware and licence first; gated models use the
person's Hugging Face sign-in or pasted token from the device vault. Either way the model is
served by a digest-pinned runtime from the signed catalog —
[llama.cpp](https://github.com/ggml-org/llama.cpp) for GGUF,
[vLLM's OpenAI-compatible server](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/)
for safetensors — supervised behind a loopback port, and a running model
becomes a model of the local provider profile that agents and Chat choose
from. Weights are data a signed runtime reads; runtimes are code and stay
signed. Why that split is safe, and the store's layout, are in
[system extensions and local models](../system-extensions-and-local-models.md#a-persons-own-models).
Downloading is an explicit management action (`install-hub-model`), granted
or denied like every other; it is never a side effect of an agent's model
request.

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

### Web search and page reading

The agent has no network of its own (`net mode = "off"`). It searches the web
and reads pages the same way it does anything else: by invoking actions. The
headless service app `apps/web-search` (`so.tilo.web-search`) publishes
`search` and `fetch` on the resource `web.search:default`. The agent finds them
through the usual discovery sequence. The agent has no tool code for them.

The service runs on the device. It needs no account, no API key and no hosted
service. `crates/platform/tilo-web-search` asks keyless search engines
directly. By default it asks Google, Brave and Bing:

- Its scrapers and merge logic are ported from metasearch2 (CC0).
- Google is asked through the search element that sites embed, a technique
  taken from SearXNG (AGPL-3.0-or-later). Google ends that for whole-web
  search on 2027-01-01.
- The crate's `PROVENANCE.md` records both sources.
- If one engine fails, the others still answer, and the result reports each
  engine's status.
- An engine that says it is getting too many requests is left alone for a
  while: 5 minutes at first, doubling up to an hour.

`fetch` only reads public `https` pages:

- It refuses local and private addresses, both before connecting and after
  DNS resolution.
- It sends no cookies.
- It caps time and size.
- It turns HTML into readable text, a window of characters at a time, so a long
  page fits the agent's result limit.

Services cannot be launched on demand, so the shell starts the packages in
`AGENT_COMPANION_PACKAGE_IDS` together with a workspace agent
(`crates/session/shell/src/shell/launcher.rs`).

Both actions are `Read` with `RiskLevel::Low` and no confirmation, because they
read public pages without signing in. Their results carry the untrusted-content
tag, because the text comes from the open web. Note that a query or an address
can carry data out. An agent that reads private data and then searches can
leak that data in its query, just as it could by opening a page in the
browser.

## Remaining integration requirements

1. **Identity and access truth** — preserve agent kind end to end; durable
   Agents page; explicit agent permission subjects; immediate revocation.
2. **Provider-neutral model protocol** — preserve the implemented Rig adapters,
   OpenAI-compatible tool calls, provider profiles, streaming and broker-backed
   credentials as new providers and hosts are added.
3. **Tasks** — issues in the Tasks app, agent runs as tracker actions
   ([tasks.md](tasks.md)); cancellation/retry only where the action contract
   permits it.
4. **Local runtime** — explicit HF snapshot lifecycle, hardware fit checks and
   supervised vLLM status.
5. **Resource/MCP projection** — publish permission-filtered resources beside
   actions and remove per-app fact tables.

Each slice must prove that revocation changes both catalogue visibility and
invocation authority. A management UI cannot claim a grant is effective until
the broker revalidates the typed agent principal against the current workspace.
