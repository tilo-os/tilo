# One action system

tilolang is not a second resource/action system beside native Tilo. It is another way to write
entries in the same one. That avoids duplicated code, and it makes interop work both ways: a
tilolang program calls native Mail exactly as Mail's palette entry does, and native code or an
agent calls a tilolang action exactly as it calls a native one.

## 1. What is already one

- `DynAction` implements `tilo_actions::Action`: same registry, envelope, executor and
  `ActionContext`.
- Records are `tilo_resource` models; edits are `tilo_resource::Edit`.
- History describes changes through `Did`; undo coalescing and search entries are the
  platform's.
- Both sides describe an app as a `tilo_actions::surface::Surface`.

## 2. Where tilolang still has its own version, and what replaces it

| Concept | Today | One system |
|---|---|---|
| Description | tilolang → catalogue only, lossy: enum parameters fall to `Array {}`, record parameters to `Text`, risk/confirmation/icon/result not set; `FieldSurface.ty` holds Rust or tilolang type text | The catalogue is the one description. `Surface` carries a language-neutral field type and value schema (from `FieldShape.schema`). A tilolang declaration is a rendering of a catalogue entry, both ways; `describe` prints native and tilolang entries alike |
| Invocation | Every tilolang change reaches the door as `tl:host/commit`, so grants are decided at that node, not the target action, resource or field. Cross-package flow calls are runtime transactions. tilolang resources are published only while a window is open | Changes are decided at the carried action's own node. Calls to another package or app are broker invocations, children of the caller (`Caller::within(reach)`); a pending call is a flow suspension. tilolang resources are published without a live window |
| Policy | The runtime's `SimpleAuthority` repeats deny lists, Outside grants and Ask; `Host` keeps a copy of agent defaults that never reads the person's agent answers; production invoker verdict is a constant Allow | `action-policy` decides, with the person's real agent answers. Code authorship is one more bound party (`Authority::bounded_by`), minimum wins. Deny lists and Outside grants are grants. The runtime's Ask becomes the door's AsksFirst |
| Refs | `ResourceRef` and `tl:<app>:<id>` strings | The catalogue's reference (`Pick::Resource`: app, kind, id, label) |
| Outbox | `$outbox` with `DynEdit::OutboxPush/Settle` | `tilo_resource`'s `Outbox<T, O>` |
| Field edits | `edit-text`/`edit-flag` at the resource node | `FIELD_EDIT` with per-path schema, decided at the field node |

What tilolang adds that the catalogue lacks stays tilolang's until the catalogue needs it:
flows, `reaches` (the static map of foreign calls, to become the up-front consent input),
search filters and sort, views (to be published as `ResourceViewSpec`).

## 3. Order

1. **Publish correctly.** Enum parameters as choices with labels, record parameters as resource
   picks, icon/result/destructive passed through.
2. **One policy.** Delete `Host`'s agent rights and the constant Allow; decide through
   `action-policy` with the person's agent answers and code authorship as a bound party.
3. **One invocation path.** Decide at the carried action's node; cross-package and cross-app
   calls as child broker invocations; headless publication. Depends on the tilolang host
   process (ui-host.md step 2b), which changes the commit path.
4. **One schema.** Language-neutral field types in `Surface`; catalogue ↔ tilolang rendering.
   `tilo_actions::field_type::FieldType` is the field type: `Model::field_type` (derived) and the
   tilolang surface generator emit it, named records and enums are declared once in
   `Surface::types`, and `tilolang_tilo::catalogue::package_of` reads any surface as a package,
   which `tilolang describe` prints and `use so.tilo.chat as chat` checks against (compile-time
   only so far). Verb parameters carry it too (`JsonSchema::field_type`, derived); a parameter
   declared only by its schema is read from the schema.

Native side, independent of tilolang: the local agent participant invents its standing instead
of using the workspace's grants and keeps no ledger, and the "ask only for a direct person" rule has
four copies. Those are fixed in the native code, since step 2 lands on top of them.
