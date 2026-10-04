# Headless adapter and receipt audit

Source-only audit after the frozen native run
`target/edited-handoff-proof/native-typed-20260916-attempt1`.
No headless run is claimed by this audit.

## Native commands and evidence

Successful commands, in order:

```sh
cargo build -p photos -p word-processor --no-default-features
cargo build -p shell --example shared-runtime-proof --no-default-features
python3 scripts/edited-handoff-proof.py --output target/edited-handoff-proof/native-typed-20260916-attempt1
```

The sibling `native-typed-20260916-attempt1-manifest.json` records the exact
process invocation, binary hashes recorded before launch, asserted properties,
explicit omissions, and hashes of all retained evidence. It leaves the original
artifact directory unchanged. Source modifications after those builds are not
retroactively covered by that run.

## Concrete headless parity blocker

Both apps use the legacy `LaunchSpec` entry point. In `ui/src/launch.rs`, the
`args.headless` branch calls `headless::run` before `run_native_app` or the custom
live handler. For Photos this bypasses `open_state` (which applies `--open`) and
`configure_app` (which registers Photos document actions). Writer's live handler
is likewise not the headless entry point.

`ui/src/headless.rs::run` opens generic persisted state, builds a `Driver`,
executes a finite input scenario, writes outputs, and exits. `FrameHarness` has
no collaboration manager, SDK action publication, incoming invocation handling,
or terminal RPC result loop. Its `Action` scenario enum supports pointer,
keyboard, semantic and accessibility input, but no typed resource invocation
with a returned `ActionOutput`.

There is a shared production dispatcher inside the headless driver. Its public
`invoke_bound_action` selects an explicit bound component destination. The
legacy Photos and Writer roots in these entry points are not such component
bindings. The unbound dispatcher calls `resolve_resource_invocation` and
`execute_resource_operation`, both `pub(crate)` toolkit internals. A proof-only
external example cannot use those APIs as a supported headless typed host.
Calling Photos rendering or Writer mutation code directly, adding a fake SDK
broker, or replaying GUI coordinates would not establish the requested parity.

The required adapter is a generic headless host that preserves the app's state
opening and action registration, discovers/publishes its real resources, accepts
typed SDK invocations, executes the existing production permission/dispatch
path, and returns actual live results. Once available, an opt-in headless mode
can reuse the native fixture, helper, negative cases, content oracle and Writer
state assertions. No such adapter is implemented in these proof-only files.

## Durable receipt output redaction

The native proof captures the live `Observe` result and
explicitly labels it as different from a durable receipt. It does **not** assert
receipt redaction or persistence.

Source evidence: `CapnpBrokerState::record_effect_receipt` reduces the terminal
update through `EffectReceiptStatus::from_terminal`; a successful status carries
no `ActionOutput`. The focused broker test
`agent_principals_delegation_profiles_and_receipts_are_owner_bound` (phase
`remote_agent_effect_terminals_become_redacted_workspace_receipts`) asserts that
success output values/resource paths and failure messages/details do not appear
in serialized workspace receipts. That unit test was not run as part of the
native proof.

An additional host limitation prevents claiming disk durability from this run:
`runtime_host::Session::start` does not set `WorkspaceTree::persist_path`, and
`persist::load_tree_from` does not restore that serde-skipped field.
`append_workspace_effect_receipt` calls `queue_save`, whose state write is
conditional on `persist_path` being set. The broker can record its redacted
in-memory receipt while this standalone host lacks the persistence binding.
No runtime-host or broker implementation was changed during this audit.
