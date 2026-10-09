# The agent action API

Tilo apps export typed actions. A program that cannot see the screen can ask the
shell what is possible right now, and then perform one of those things by name.
No screenshot, no hit-test, no synthetic click.

Two ops carry this, on either control socket: the device admin's (below) or a
development session's `TILO_TEST_CONTROL` socket. Both use the existing
newline-delimited JSON framing described in `docs/headless-testing.md`: one JSON
object per line, request and response.

## Driving a device as its admin

Every compositor build serves the action model at
`$XDG_RUNTIME_DIR/tilo-session/admin-control.sock` in the device admin's
session, once first login is complete. `tilo-control` (in every image) uses it
by default for `actions list|invoke|status|cancel`, `windows` and `state-dump`:

```bash
ssh admin@device
tilo-control actions list notes
tilo-control actions invoke applications so.tilo.notes   # typed receipt
tilo-control windows                                     # the window it opened
tilo-control actions invoke files /home/admin/Documents/report.md
tilo-control actions invoke session lock
```

Nothing about the session changes because the socket exists: first-use
onboarding, the lock screen and the capability gate behave exactly as with no
client attached. While the session is locked, ordinary app invocations are
refused with `session_locked`. The authenticated owner's `session-lock`
provider remains available for the normal PAM unlock conversation.

Who may use it is decided by the system's own identity and process model, not
by a token:

- **Only the device admin's session binds it**: an account in `wheel` whose
  active identity is the one that claimed the device at its first login and
  holds its primary device certificate
  (`tilo_identity_host::provisioning::active_identity_owns_device`). Another
  account on the machine, or the admin before first login, has no socket.
- **Only the admin's own unconfined processes connect.** Each connection is
  checked against the kernel's `SO_PEERCRED`: same uid as the session, same
  user and mount namespaces (a sandboxed app has its own, and is never shown
  the path), and not a process this session launched, confined or not. An app
  acts through its own collaboration connection and grants, never with the
  admin's. The socket is 0600 in a 0700 directory besides.
- **It serves the action model only**: enumerate, invoke, status, cancel,
  `state_dump` and `foreign_frames`. Synthetic input, screenshots, grants,
  fixtures and agent seats are refused with `not_permitted`, naming the
  development socket that serves them.
- **Every invocation passes the same capability gate** as the palette, as the
  person at this session.

### Unlocking the owner's session remotely

Use the owner's existing unconfined login and the owner socket. Discover the
current `session-lock` actions, then submit the `password` Secret parameter to
`unlock@session-lock` as a private JSON object on standard input:

```bash
env -u TILO_TEST_CONTROL tilo-control \
  --socket "$XDG_RUNTIME_DIR/tilo-session/admin-control.sock" \
  actions list session-lock --json
env -u TILO_TEST_CONTROL tilo-control \
  --socket "$XDG_RUNTIME_DIR/tilo-session/admin-control.sock" \
  actions invoke session-lock unlock@session-lock --args-stdin
```

Supply stdin through the existing private credential channel, never a password
in arguments, receipts or logs. The invocation starts the same PAM verifier
as the lock screen; its acknowledgement does not prove authentication succeeded.
Inspect `chrome.session_locked` and `chrome.lock_auth` in `state-dump` to observe
completion. An incorrect credential leaves the session locked.

The management transport `tilo-vm vm ssh` executes commands as root. When using
that transport, run the control client as the actual session owner with
`runuser -u OWNER --`, set that owner's `XDG_RUNTIME_DIR`, and keep the secret on
stdin. Root itself does not pass the owner socket's peer checks. This unlocks an
existing login session; the pre-session greeter has its separate sign-in action
below, and encrypted-disk unlock is a separate boundary.

Permission requests use that same authenticated session surface. When a portal
or Terminal is waiting for consent, query `actions list permission-prompts` and
use the returned exact UUID-scoped decision: `allow-once@<id>`,
`allow-session@<id>`, `allow-always@<id>` or `deny@<id>` from provider
`permission-prompts`, with `{"confirm": true}`. Only the current queue head is
offered; listing and invoking require the current workspace owner. The action
rechecks ownership and request identity before releasing the waiting request.
It cannot create a grant without a prompt, answer a replaced request or reopen a
Host session as a person on an agent's behalf. The portal/ush supervisor still
owns private persistence and Host's existing person/device-owner checks.
A successful decision receipt confirms the answer, not a running PTY: inspect
Terminal's local session afterward to observe its actual startup outcome.

Reading is an action too. A framework resource that sets
`Resource::READABLE` and answers `Resource::read_content` is published with
the framework's `read` view: no arguments answers the resource itself, `item`
answers one nested resource it lists. It needs read permission only, passes
the same gate as every other action, and is tagged as the document read, so
the runtime MCP finds it too. Notes answers its pages and tags, and one page
(`notes.page:<id>`) as its title, text and blocks with the ids its edit
actions take:

```bash
tilo-control read so.tilo.notes                    # {"pages": [{"item": "notes.page:…", "title": …}], …}
tilo-control read so.tilo.notes notes.page:<id>    # {"title": …, "text": …, "blocks": [{"id", "kind", "text"}]}
```

`read APP [ITEM]` finds the open app's `read` row among the `app-actions` the
desktop lists and invokes it, so the caller needs no opaque id.

The CLI teaches its own use: `tilo-control --help` leads with the action
model, a socket it cannot reach is explained (and how to get one), and
`unknown_action` / `invalid_arguments` refusals print the next command to try.

`os/tests/installed-admin-actions.sh` proves this on a booted image in
`build-image.yml`: after first login, over the admin's ssh and with no
`TILO_TEST_CONTROL`, it invokes Notes by address, lists it, and reads the window
back through `tilo-control windows`. It then creates a note through Notes' `new-page` action and reads it back through `read`.

## `actions_enumerate`

```json
{"op":"actions_enumerate","query":"terminal"}
```

```json
{"answer":{"query":"terminal","count":2,"actions":[
  {"provider":"actions","id":"window.new.terminal","kind":"Actions",
   "title":"New terminal window","subtitle":null,"score":0}
]}}
```

Machine-oriented operations can be included explicitly through the same door:

```sh
tilo-control actions list so.tilo.browser --programmatic --json
```

```json
{"op":"actions_enumerate","query":"so.tilo.browser","include_programmatic":true}
```

This adds programmatic descriptors to bounded enumeration. It keeps the same
workspace, membership and capability checks and does not alter the human palette.
Use the returned provider, ID, contract and parameter schema with `actions invoke`.
A programmatic page read may carry Write authority; the generic `read` convenience
command is limited to declared read-only operations.

The providers on a stock build are `actions` (the shell's own chrome),
`applications`, `host-applications`, `notes`, `files`, `records` and `editor`.

An empty `query` lists every provider's default set rather than guessing a
needle. The descriptors come from the same provider registry that backs the
Ctrl+K command palette — the palette renders these as rows, enumeration renders
nothing.

## `actions_invoke`

```json
{"op":"actions_invoke","provider":"actions","id":"window.new.terminal"}
```

```json
{"answer":{"provider":"actions","id":"window.new.terminal",
           "kind":"Actions","title":"New terminal window"}}
```

An address that is unknown, or hidden from the caller, is refused — and the two
are deliberately indistinguishable, so probing cannot map what you may not see:

```json
{"err":{"code":"unknown_action",
        "message":"no visible action addressed by (actions, nope) in the current catalogue"}}
```

An application action answers once it is admitted: its receipt carries the
broker job (`broker_invocation_id`) and usually the state `accepted`, and
`actions_status` follows the job from there. `wait_ms` holds the answer instead
until the job settles — succeeds, fails or is cancelled — or the wait runs out
(capped at 30 s), and answers with the job's latest receipt either way. The
desktop is woken by the job itself, not polled, so the answer leaves as soon as
it settles:

```json
{"op":"actions_invoke","provider":"app-actions","id":"…","wait_ms":5000}
```

```bash
tilo-control actions invoke app-actions "$ID" --wait        # 5 s; --wait=MS for another bound
```

`--wait` exits 1 when the action failed or was cancelled, and names the job to
follow with `actions status` when it is still running. Without `wait_ms` the
answer is the admission receipt, as before; with `device` it is ignored.

A client making many calls can keep one connection: `tilo-control actions
session` reads one wire request per stdin line and writes one answer per stdout
line, in order, over a single connection.

## Reading a terminal session

For Terminal, `tilo-control read so.tilo.terminal --json` returns the shared
session target, screen and cursor, with an explicit truncation flag when output
exceeds 64 KiB. That record can outlive the process or describe a collaborator's
screen. Discover and invoke the read-only **Inspect session** action for this
participant's actual lifecycle, attached process identity, input availability
and startup cause. Neither getter opens, restarts or grants a session. A command
is unavailable until the local process has opened; sending to a placeholder
writer cannot report success. The device's own shell continues to require the
person, device ownership and an explicit grant; agents use managed environments.

## Why enumeration and invocation are separate steps

They answer different questions and carry different risk. "What can I do here"
must be free to ask; "do this" must not be.

Collapsing them into one call — a search that runs its best match — makes every
query a potential side effect, so a caller exploring the surface cannot explore
it safely. Keeping them apart means a caller can inspect what an address
resolves to, reason about it, and only then commit. It also makes the failure
modes distinguishable: "that action does not exist for you" and "I ran it and
nothing happened" must not look alike to a caller with no way to check the
screen.

Invocation re-resolves the address against a freshly built catalogue before
anything runs, so an address that was valid at enumeration time and is no longer
valid now fails loudly instead of running something else. The request runs from
that one resolution: nothing else runs between resolving the address and
invoking it.

## Why enumeration carries no invoke handle

The descriptors are inert data: `provider`, `id`, `kind`, `title`, `subtitle`,
`score`. There is no callback, no token, no handle. Read-only is a property of
the reply's shape, not a promise in a comment — there is nothing in an
enumeration result that could be fired, so surveying the surface cannot cause an
effect even by mistake.

An action is therefore addressed by a stable `(provider, id)` pair and never by
a screen position. That is what makes the surface drivable by something that
cannot see, and what makes an invocation reproducible across window layouts,
resolutions and themes.

## The visibility gate

`ActionCatalog` (`crates/session/shell/src/shell/search/action_api.rs`) applies an
`ActionVisibility` gate when it is built, and keeps the result as an index list
of the entries that passed. Every public accessor — `enumerate`, `select`,
`invoke` — reads through that same list.

The consequence is the important part: a denied action is *absent*, not
*disabled*. It does not appear in enumeration, and `select` on its address
returns nothing, so `actions_invoke` refuses with `unknown_action`. A caller
cannot invoke what it was never shown, and cannot learn that a hidden action
exists by trying to run it. Contrast the usual arrangement, where a filtered
list and an unfiltered dispatcher disagree and the list is merely advisory.

The implementation the socket passes is `CapabilityVisibility`
(`crates/session/shell/src/shell/search/visibility.rs`), which resolves the active workspace,
builds the actor for the local user, and asks that workspace's own capability
model. It is fail-closed twice over: `decide_capability` denies unless something
explicitly permits, and a session that cannot resolve a workspace at all errors
rather than falling back to showing everything. A gate that opens when it cannot
find its policy is not a gate.

### What is gated, and what is not

The rule is the one `crates/session/shell/src/shell/actions.rs` already states, not a new one:
showing a resource is non-destructive and needs no permission; taking an action
on one requires permission. So notes, records, files and folders are never
hidden, and neither are the actions that merely open an overlay —
`settings.open` reveals a panel, it does not change a setting. What is gated is
acting: `Write` for spawning a terminal, duplicating a tab or launching an app,
`Invite` for peer operations. An action no mapping covers is treated as an act
requiring `Write`, so a provider that lands without one goes dark for a
restricted caller instead of being quietly reachable.

The effect is observable on a live session. Against a workspace the local user
owns, `actions_enumerate` returns the full surface — 191 entries on a stock
build. Against a workspace owned by someone else, the same call returns only
what anyone may see: the overlay-opening actions and the documents. No error, no
empty list, no hint that the rest exists.

## Pointing Claude Code at it

`scripts/tilo-mcp-server.py` is an MCP stdio server exposing the typed Tilo
tools and a lazy resource tree. `resources/list` returns only application
surfaces at `tilo://application/<app>`; it does not put every action schema into
the model's initial context. Reading one application returns the objects it
currently publishes with each object's full action descriptors nested beneath
it.

This nesting is the resource/action model rather than an MCP-only taxonomy:
an application owns resources (the object a person would recognise, such as a
file or document), and each resource owns the operations valid for it now.
Shell palette providers that do not publish a resource tree remain reachable
as one provider application with one invokable operation per object; the shim
does not infer a fictional hierarchy from dots in their display ids.

Every level re-enumerates through the visibility gate before answering. A
closed object or revoked action therefore disappears from its parent and a
stale URI fails as unknown; nesting changes when descriptors enter context,
not what the caller is allowed to reach. Standard library only, no SDK.

Start a compositor, then register the shim:

```bash
export TILO_TEST_CONTROL=$XDG_RUNTIME_DIR/tilo-session/test-control.sock
cargo run --bin shell -- --backend headless &

# Verify the shim reaches the compositor before putting a model behind it:
scripts/tilo-mcp-server.py --selftest

claude mcp add tilo -- /absolute/path/to/scripts/tilo-mcp-server.py
```

The socket path comes from `$TILO_TEST_CONTROL`, or `--socket`. The shim never
fabricates an action list or a success: if the compositor is unreachable, or
answers `{"err": ...}`, the model receives that as a tool error with the
compositor's own code and message intact.

## Apps that run before the compositor

The installer and the greeter run under `cage` straight from greetd, so no
compositor socket reaches them. They serve the same three semantic ops
themselves — `actions_enumerate`, `actions_invoke`, `state_dump` — through
`crates/session/console-control`, so `tilo-control --socket <path> actions
list` drives them unchanged:

- on the machine, `$XDG_RUNTIME_DIR/tilo-console/<app>.sock` (0700 directory,
  0600 socket) for the local console and assistive tooling of that user;
- in a VM, the virtio-serial port `org.tilo.console.0`, which exists only when
  the hypervisor attaches it (`os/dev/vm.sh console` prints its host end).
  Physical machines have no such bus; nothing listens on a network.

Action ids there are `<action>@<resource>` (`next@installer`,
`begin@installer.review`). A contract that asks for confirmation takes it as
an argument, the same projection the compositor publishes for app actions:
`confirm: true` for `Confirm`, `confirmation_phrase` for a typed `Phrase`.
An action tagged `confirmation:by-argument` (a framework app's generic `apply`
edit) advertises an optional `confirm`: the app classifies each value, and a
destructive one — deleting a clip — is refused with
`action_confirmation_required` until it is resent with `confirm: true`.
Input injection is not served; the surface is semantic only.
`tilo-vm scenario install` drives the installer this way end to end
(`tilo-vm guide scenarios`).

The greeter publishes `sign-in@greeter`, account and session selection, and
the available fingerprint action. Sign-in uses the existing greetd/PAM
conversation and the selected installed session; its `password` parameter
is `Secret`. Use `actions invoke greeter sign-in@greeter --args-stdin` with
a private JSON object on standard input, rather than placing a credential
in the command line. An accepted invocation means the attempt started;
`state-dump` reports authentication progress and the mapped result.

For a bounded timing measurement, `configure-session-diagnostics@greeter`
configures this greeter's next ordinary sign-in with four required booleans:
`presentation`, `gpu_stages`, `gpu_timestamps`, `gpu_dispatch_timestamps`.
Dispatch timestamps require GPU timestamps. The selection is transient and
inspectable as `session_diagnostics`; a fresh greeter starts without a selection,
and normal diagnostic flags are off unless explicitly configured by the host.
Set all four false to disable them for this sign-in. Authentication in progress
refuses changes. These are fixed content-free timing switches, never arbitrary
environment, session argv, output paths or input gates. Invoke normal `sign-in`
through private stdin afterward; PAM and the installed session remain authoritative.
No measurement qualifies without the matching actual input, body revision and
hardware presentation proof; GPU-feature support must be observed separately.

If PAM requires another answer, the current challenge has a unique ID and
a `secret` or `visible` response kind, and its bounded normal PAM question
with literal echoes of private answers removed. `respond-challenge@greeter` takes that
ID as `challenge` and a private `Secret` argument named `response`. Stale
IDs cannot answer a later prompt. `cancel-authentication@greeter` sends the
ordinary greetd cancellation request and keeps the attempt busy until its
worker finishes; no cancelled response can start a session. Credentials
and raw PAM diagnostics never enter resource values, encoded operations,
state dumps, action replies or logs.

## Carrying one action result into the next

An action sequence is a data path, not just a list of commands. The broker's
terminal `Succeeded` update contains the declared `ActionOutput`; the agent
retains that output for exactly the immediately following step. Content bytes
are represented by one canonical resource address, `tilo.content:<hash>`. An
action built with `prompted_content` consumes that hash from the preceding
application result and replaces any value the model tried to provide.

Local collaborative-document actions use the same rule through the owning
application's declared product function. The agent does not inspect a document
and guess which bytes it represents. Missing content, the wrong resource kind,
or more than one content result stops the sequence rather than selecting one.

This hand-off is deliberately not a workflow language: there are no named
variables, JSONPath expressions, graphs or implicit reuse of an older result.
The narrow one-step hand-off is enough to express a cross-app operation such as
“rotate the photograph, then place the result in the document”, while keeping
the origin of the content mechanically checkable.

Saved, multi-step pipelines are shortcuts (`crates/collab/automation`,
`tilo-automation`): named steps with typed bindings between them, checked at
save time against the same descriptors this API enumerates and run through
the same `ActionHost` door as the invoking person, inside the shortcut's own
grant.

## AI transforms

The workspace agent (`so.tilo.agent`, running with `--task-runs`) publishes
one programmatic resource, `agent.transform:default`, whose actions any caller
invokes like any other app's: `summarise`, `extract`, `classify` and
`freeform`. A shortcut's `Ai` step lowers to exactly these.

Each takes its text as `input`, or as a `tilo.content:<hash>` address in
`resource`, and declares a JSON result: text for `summarise` (bounded by
`length`), a label (or with `multi` a list of labels) for `classify`, a value
satisfying the caller's own `schema` for `extract`, and text or a value of the
caller's `schema` for `freeform`. The agent validates the model's answer
against that shape before returning it; an answer that misses gets one repair
attempt, then fails with `invalid_model_output` rather than returning a guess.
Other failure codes are `invalid_arguments`, `input_too_large`,
`content_unavailable`, `model_unavailable`, `timed_out` and `busy`.

The caller never reaches a model. The agent asks the model gateway for one
bounded single-shot answer (`Request::Transform`, admitted only for
`so.tilo.agent`); the gateway builds the provider request, caps input, output
tokens and deadline, and returns only the answer text. A shortcut cannot feed
a secret to a transform: the checker refuses the binding and the interpreter
refuses it again at run time.

## Source of truth

| What | Where |
|---|---|
| Command + response shapes | `crates/session/shell/src/compositor/test_control.rs` |
| Device admin's control socket | `crates/session/shell/src/compositor/admin_control.rs` |
| CLI (`tilo-control`) | `crates/session/control-cli` |
| Launcher coverage proof | `crates/session/shell/src/shell/overlay_host/launcher_action_coverage_tests.rs` |
| Live enumerate / resolve | `crates/session/shell/src/compositor/native.rs` |
| Catalogue + visibility seam | `crates/session/shell/src/shell/search/action_api.rs` |
| Capability gate + policy | `crates/session/shell/src/shell/search/visibility.rs` |
| Live end-to-end proof | `scripts/headless-examples/action-api-demo.py` |
| Cross-app result hand-off proof | `apps/agent/tests/discovered_operations.rs` |
| Shortcut pipelines (check + run) | `crates/collab/automation` |
| Keyboard shortcuts (`keyboard-shortcuts`: list, rebind, reset) | `crates/session/shell/src/shell/keyboard_shortcuts.rs`, catalogue in `crates/system/settings-store/src/keyboard_shortcuts.rs` |
| AI transforms | `apps/agent/src/transform.rs` |
| MCP shim | `scripts/tilo-mcp-server.py` |
| Console apps (installer, greeter) | `crates/session/console-control` |
| Socket framing, other ops | `docs/headless-testing.md` |

### Unlocking the device-owner session

The existing device-owner `admin-control.sock` publishes the `session-lock`
resource while locked. Invoke `unlock@session-lock` with a Secret `password`,
using `tilo-control actions invoke session-lock unlock@session-lock --args-stdin`.
Supply the JSON object through private stdin; never place a credential in argv.
The reply acknowledges asynchronous verification, rather than claiming unlock.
Wait for the ordinary `state-dump` `chrome.session_locked` transition and
`chrome.lock_auth.verifying`/non-leaky error fields. The normal PAM helper
verifies the existing session owner; no caller may replace the username.
Agent-scoped and enrolled-device actions do not receive this local capability.
Other app actions remain refused while locked.
## This device's shell

A live launcher-authenticated Terminal exposes owner operations through the
Shell's `terminal-device-shell` provider. List `device shell` to obtain the
exact per-connection addresses. Invoke `request@<connection>` with
`confirm: true` to select the ordinary Host target. This does not grant it:
answer its existing permission request using `permission-prompts`, then use
`inspect@<connection>` to obtain the current local session UUID.

`run@<connection>` requires that exact `session`, a single reviewed `command`
(up to 8 KiB), and `confirm: true`. Its acknowledgement says `submitted` only
after the existing PTY accepted the bytes; it does not claim an exit status.
`read@<connection>` requires the same session UUID and returns at most 32 KiB
of visible output, including retained output after the process exits. Read
and inspect expose no clipboard, credential or arbitrary host-file API.

Only a person administrator/owner may use this provider. App, agent and token
principals remain refused even with an Admin override. Generic app-broker
Terminal operations cannot open Host or send input to an existing Host PTY;
Default environments retain their normal app actions. Closed connections,
changed sessions, missing acknowledgement and unavailable PTYs fail visibly.
The matched Shell, SDK runtime and Terminal versions are required for this
owner lifecycle operation; unsupported workers do not report success.

### Workspace membership and invitations

The `actions` provider publishes workspace membership under the canonical ids
and parameter names of `tilo_action_broker::collaboration_admin`, acting on the
active workspace:

| Action | Parameters | Output |
| --- | --- | --- |
| `member.role.set` | `participant_id`, `role` (`viewer`, `commenter` or `editor`) | none |
| `member.remove` | `participant_id` | none |
| `invitation.issue` | `role`, `expires_in_seconds` (1 to 30 days), optional `invitee_name`, optional `recipient` | `{"invite": token}` |
| `invitation.list` | none | `{"invitations": [{invite_id, name, role, expires_at}]}` |
| `invitation.resend` | `invite_id` | `{"invite": token}` |
| `invitation.cancel` | `invite_id` | `{"canceled": true}` |

Each also takes `confirm: true`; the contracts are privileged, sensitive or
destructive and require `Admin` on the workspace. `participant_id` is the
member's person id as the roster reports it. Setting a role only changes an
existing member's role; admitting someone new is an invitation. `recipient` is
the invitee's 64-hex person key (or a `tilo-identity:` card); with it, only
that person's devices can redeem the token, which is otherwise a bearer
credential. The invitation actions need peer sync and complete on a later
frame, so callers poll the pending invocation. The Members tab in workspace
settings calls the same functions as `member.role.set` and `member.remove`, so
a click and an invocation pass the same authority check.

### Reviewing a Tilo package in Store

Store accepts `.tapp` files through the normal file-open handoff, its shared
“Review package” file chooser, and the declared `import-package` action. Discover
Store's current bound resource/action IDs before invoking them; resource IDs are
session-bound. The action accepts either a sandbox-readable `path` or a handed
`content` hash, never both. Omitting both opens the ordinary file chooser.
Opening or reviewing a file does not install it. The optional `reported_size`
is a preflight refusal hint for oversized handoffs; it never replaces independent
byte bounds, archive verification or the repository offer size.

Full and thin archives use the same review: every carried payload is hash-checked,
then signed metadata is matched to an independently verified configured
repository offer (app, version, root and publisher). The independently authenticated
offer size is retained for the installer's exact download-size check; full and thin
local files can encode the same identity at different lengths. An archive's
own key never creates publisher trust. Unlisted packages require independently
provisioned identity-scoped trust, as described below; component and runtime
archives use the system updater.

After the review job succeeds, `inspect-catalogue` exposes `imported_package`
with its app, root and file version. The app details show that version, its signed
identity and whether the app was already installed. Invoke the separate
`install-imported-package` action with that exact `app` and `root`, or activate
“Install reviewed package” in those details. Store rechecks the current offer
before passing it to the existing authenticated `InstallOffer` installer. The
installer obtains the canonical offer and validates its runtime and device target;
reviewing a full file does not bypass fetching or authorize an unavailable runtime.
Poll each numeric broker job to a terminal outcome, then separately inspect the
selected app's installed state before invoking its ordinary Open action. A
pre-existing installed app is an update/reuse test, not a first installation.

### Trusted local `.tapp` files in Store

Store's existing **Review package** and **Install reviewed package** actions also
accept unlisted local app packages when the user's host-side
`~/.config/tilo/trusted-attestors.toml` already authorizes a valid scoped
attestation. The host checks `revoked-keys.toml` before honoring that attestation.
Opening the file never grants publisher trust or installs anything. Valid
self-signatures alone are refused on this Store path, even though the standalone
installer's default policy can warn and continue.

The existing launcher-issued AppHost capability reviews handed bytes without
receiving a publisher key or host filesystem path from the worker. Installation
rechecks the exact reviewed archive digest and signed root against current host
trust, then calls the canonical local installer with its verified attestor
requirement. A publisher change is refused; normal pinned/held selection and
runtime/interface policies remain in force. A thin local file needs its missing
payload already in the canonical content store; Store does not invent an
untrusted download source. The review reports actual installed state separately
from the candidate, and successful installation requires the reviewed root to
be the selected installed package.
### Timing diagnostics in an active owner session

The authenticated local owner socket publishes `session-diagnostics` with
`configure@session-diagnostics` and `inspect@session-diagnostics`. Configure
requires exactly four booleans: `presentation`, `gpu_stages`, `gpu_timestamps`,
and `gpu_dispatch_timestamps`. Dispatch timestamps require GPU timestamps.
Use the ordinary `tilo-control actions invoke` argument JSON; no environment
names or output paths are accepted.

The setting is ephemeral to this desktop session. It sets or removes only the
four existing diagnostic variables for subsequently launched children.
Presentation also controls the compositor's existing physical input delivery
observer immediately. Existing applications and the compositor's initialized
GPU renderer retain their configuration; reopen the application to collect
its new timing stream. Inspect returns the current fixed options and their
scope. Agent-scoped, sandboxed, other-account and locked-session calls cannot
configure this owner-only surface. No application content or typed keys are
recorded by these timing options.

## Cloud account mounts

Settings publishes `sync-now` and either `pause-sync` or `resume-sync` on each
`settings.account:<id>` row while the cloud account service lists that account
as mounted at `~/Remote/<label>`; an account that is not mounted offers none of
them. They are device-only jobs: the effect answers once the service has
accepted the request, and the account's health still arrives through the
account status store. `sync-now` cuts a failure back-off short and runs a pass
(an account whose mount could not start is tried again); a pause stops
background sync and uploads until resumed, survives restarts, and leaves
on-demand reads working while edits queue.

Settings reaches the mounts through the service's account socket,
`$XDG_RUNTIME_DIR/tilo-fs-cloud/control.sock` (`runtime_fs::cloud`, mode
0600), whose directory its sandbox is granted. That socket lists, syncs,
pauses and resumes mounts and refuses everything else. File requests (a
file's provider revisions, restoring one of them, and a share link, each
refused unless the provider's capabilities include it) are served only on the
file socket, `$XDG_RUNTIME_DIR/tilo-fs-cloud-files/files.sock`, in a directory
of its own that no grant of the account socket reaches. No credential crosses
either socket. Files does not yet publish these file requests as entry
actions, and no sandbox is granted the file socket yet.
