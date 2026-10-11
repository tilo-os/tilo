# Prove an app's public workflow

The small checklist in
[`resource-journey.rs`](../crates/sdk/tilo-app/examples/resource-journey.rs)
starts empty. A shared text input edits its draft; Add creates a task with a
stable id and the host-provided author. Each task publishes completion and
confirmed removal actions. It uses shared controls and Fluent strings, with no
custom window chrome, storage, permission service or identity provider.

Run it with Cargo or any Cargo drop-in:

```sh
cargo run --locked -p tilo-app --example resource-journey
cargo test --locked -p tilo-app --no-default-features --example resource-journey -j 2
```

`TestApp` mounts the actual component runtime on a temporary profile. The example
journeys exercise both a bound input control and the semantic surface agents,
scripts and accessibility use:

1. Refuse Add when the draft is empty; write no version for the refusal.
2. Create a task through its actions and attribute it to the current person.
3. Discover the task's `set-done` descriptor with `app.actions()`. Its address
   already binds task identity; the caller supplies only `done`.
4. Refuse a string where the action requires a boolean, then complete the task.
5. Discover Remove, inspect its confirmation contract, and prove an ordinary
   `call_on` refuses before `call_confirmed_on` admits that invocation.
6. Refuse the deleted task's stale address; undo the removal and recover the
   same address. A subsequent removal still needs confirmation.
7. Reopen and recover completion and authorship without an app-specific save.
8. Change the live role to Viewer. Remove disappears from discovery and even a
   confirmed call using a previously discovered descriptor is refused. Restore
   Owner and invoke the confirmed root action with an explicit task id.

`actions()` returns `Result<Vec<ActionDescriptor>, DoorError>` in address order.
It reads the production permission-filtered component catalogue and propagates
observation errors rather than treating them as an empty surface. Descriptors include
parameter schemas, authority requirements, confirmation, execution and result
contracts. `agent_surface()` retains its compact snapshot format. Discovery
carries no authority: the host checks current permissions and arguments at each
invocation. `call_confirmed` and `call_confirmed_on` represent an explicit
confirmation in a test; they use the existing host dispatch, not a second
executor or a persistent permission grant.

The tests prove the production component and document paths headlessly. They do
not certify a live compositor connection, transport between devices, device
identity authentication or a screenshot in each theme. `for_person` supplies a
test participant id; it does not sign someone into the real OS.

The `agent_execution_merges_with_person_edits_and_survives_undo_and_reconnect`
journey executes a task's discovered action through `Participant`, rather than
stopping at discovery. A concurrent person edit survives delivery of the agent's
edit; the history keeps the agent's attribution, and the person's semantic undo
and redo retain the agent's work. The agent restores its saved replica, receives
the newer snapshot and checks convergence. Revoking its in-memory fixture grant
refuses the remembered address without writing history. Reopening the app retains
the same tasks and attributed versions. This is a resource/runtime fixture, not a
device sign-in or a persistent grant.

## Permission-aware views

Bound controls already follow the permissions of their actions. For an entire
pane or a permission-specific explanation, observe the host instead of copying
the participant's role into the shared model:

```rust
let admin = cx.permission_state(PermissionRequirement::Admin);
let owner_tools = cx.switch(&admin, |_, allowed| {
    if *allowed {
        Box::new(Text::new("app-owner-tools"))
    } else {
        Box::new(Column::new().with_intrinsic_height().with_no_padding())
    }
});
```

Use your app's Fluent key for the label and put `owner_tools` into its view.
The state updates when the host changes permission, including Owner to Editor
(writing stays allowed) and Viewer to Commenter (writing stays denied). No
resource edit, reopening or app-owned role tracker is needed. Custom surfaces
can obtain the same state from their dispatcher. History views have no live
owner authority.

This is presentation state. Resource actions and effects still declare and
enforce their own permission requirements. Keep device-private review data out
of the shared resource; hiding a pane does not remove data from a replicated
document.

The production [`live_permissions` tests](../crates/sdk/tilo-app/tests/live_permissions.rs)
exercise first-frame restrictions, revocation without a document change,
restored access, refused owner actions and rendering a past version.

## Platform lesson and sources

SvelteKit's [form actions](https://svelte.dev/docs/kit/form-actions) use the
browser's form submission mechanism and report validation failures through the
same action path. Its [auth guidance](https://svelte.dev/docs/kit/auth) and
[error handling](https://svelte.dev/docs/kit/errors) describe the web host's
responsibilities. The useful design lesson here is to make the platform's actual
workflow easy to express and prove. Tilo already owns resource identity,
permission checks, history and persistence; app authors should exercise those
services directly rather than build web-style sessions or a parallel test model.

The coordinating session verified the [Karatzas reference](https://x.com/karatzas_thomas/status/2105691407524802564):
progressive API disclosure from a basic Save button, to a system icon, to custom
label content, then a UIKit escape hatch. Direct fetching in this execution
session returned HTTP 403. The example applies that supplied lesson through
existing Tilo APIs: `cx.button`, an icon-only shared `Button`, an icon plus a
custom Fluent label, and a custom component implementing `Decorator` over a
bound button. A third journey inspects the production control inventory and
asserts that each presentation binds exactly the same discovered Add address.
It proves their correct spoken names (including the custom visible label), button
roles, keyboard focus, each Enter activation, undo and live disabled state.
Revoking permission between focus and Enter refuses every presentation without
a state or history change.

Apple's [Button documentation](https://developer.apple.com/documentation/swiftui/button)
describes an action paired with a label view (text, icon or both), while
[UIViewRepresentable](https://developer.apple.com/documentation/swiftui/uiviewrepresentable)
wraps a UIKit view in the SwiftUI hierarchy. Those primary sources support the
progression; they do not imply that Tilo should adopt UIKit or app-owned pixels.

This is a presentation progression, not full SwiftUI parity: Tilo's current
button labels accept text and an icon, not an arbitrary child widget tree. A
UIKit-style platform bridge is also outside this example. Those deeper control
extensions should be coordinated with the UI crate split rather than introduced
as a second control implementation.

The supplied summary of [Daniel's Swift/Opus reference](https://x.com/imnotchalk/status/2106876459625451547)
reinforces the value of native defaults for generated UI. That URL also returned
403 here, so this work does not attribute additional claims to the post. This
example inherits typography, spacing, focus and accessibility from shared
controls; visual quality across themes remains a separate verification step.

The headless build also exposed a mismatched feature gate: the SDK exported
`definition_launch_spec` without `native` although its implementation requires
that feature. The export and its native-only preparation helper now use the same
gate as the implementation, allowing the runner-free test configuration to compile
without an unused-helper warning. The no-runner executable boundary also lacked
the native entry point's documented exception for its intentional failure exit;
its lint expectation now states that purpose explicitly.

The walkthrough uses the SDK's action and view contracts. Testing delegates to
the existing component harness, and permission-aware presentation observes the
host's existing permission checks. These contracts let the UI split change its
internal crate layout without requiring applications to rebuild platform
services.
