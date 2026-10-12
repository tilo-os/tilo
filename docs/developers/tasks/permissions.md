# Follow the host's permissions

Retrieve this task with `tilo-app guide permissions --json`; `roles` and
`access` are aliases. No project, network connection or full manual is needed.

Keep permissions on the resource action. Every action has a kind the
framework derives: a view reads (`ActionKind::Read`), a verb or field edit
changes the document (`ActionKind::Undoable`), and an effect or a verb that
queues an outside consequence reaches outside it (`ActionKind::Outside`). The
room's grants answer each kind at each place in the published tree: allow, ask
first, or deny. `#[action(field = "comments")]` puts a verb under the comments
a commenter may add. Bind controls to the generated descriptors: they follow
current access automatically. Scripts, agents and accessibility invoke the
same checked action.

For an owner-only pane or a permission-specific explanation, observe live
host state from the view instead of storing a role in the shared model:

```rust
let admin = cx.govern_state();
let owner_tools = cx.switch(&admin, |_, allowed| {
    if *allowed {
        Box::new(Text::new("app-owner-tools"))
    } else {
        Box::new(Column::new().with_intrinsic_height().with_no_padding())
    }
});
```

`cx.permission_state(ActionNeed::of(ActionKind::Outside))` observes whether
the participant may reach outside the document here in the same way.

Put `owner_tools` into the view and declare `app-owner-tools` in your Fluent
locale. The pane follows revocation and restoration without reopening the app
or changing its document. An editor still changes the document when
governing the room is taken away. History views have no live owner authority.

Hiding a pane is presentation, not authorization or data isolation. Actions
and effects must enforce their own requirements. Device-private information
must stay outside the replicated resource. The host supplies authenticated
identity and workspace membership; an app does not grant itself a role or
create a separate sign-in service. An agent is an ordinary collaborator.

In `TestApp`, call `set_role` while the app is open. Check the pane disappears,
`invoke` refuses its restricted action and the resource stays unchanged; then
restore access and check the action succeeds. Also test an unprivileged first
frame and a history view. `for_person` supplies test attribution, not real
sign-in. Authenticated membership and device delivery need host integration
tests.

Next: `tilo-app guide actions` or `guide collaborate`.
More: [Executable live-permission tests](../../../crates/sdk/tilo-app/tests/live_permissions.rs),
[Resource workflow](../../sdk-resource-journey.md#permission-aware-views).
