# Feedback

Feedback is an attributed report in a configured Chat channel. It uses the
channel's ordinary permission-checked action transport; it creates neither an
account connection nor a second messaging service. A report's resource is
`chat.feedback:<submission-id>`, beside its `m_feedback_<submission-id>` message.

## Implemented contracts

`tilo-feedback-review` supplies deterministic privacy review. Model proposals
can add redactions but cannot remove the deterministic floor. Invalid spans or
an oversized merged floor fail the review rather than silently losing masks.

The SDK exposes `tilo-feedback` as `ui::feedback`, supplying the portable draft, approved preview, capture boundary
and durable outbox. Drafts begin with no context. The host attaches context only
after an explicit user action. Semantic identifiers stay local; context sent in
a report has synthetic field keys. Credentials and declared personal values are
masked before review output can enter the outbox. Further redaction or an
attachment change requires fresh approval.

A screenshot is a one-shot request for a specific app surface and semantic
revision. A changed or protected surface, unknown sensitive geometry, invalid
pixel bounds or oversized image is refused. The host must classify all text
bounds, treating unclassified text as sensitive. Masking precedes PNG encoding;
only the resulting content-addressed PNG is eligible for the attachment store.
There is no desktop capture or automatic recording buffer.

The outbox uses the host's existing `DurableStore`. Sending waits for local
persistence. Retried reports preserve the same submission identity and payload,
including across restarts. A successful invocation is not delivery: the outbox
requires a receipt from the authenticated destination with matching identity,
destination and payload digest, and persists that receipt before showing Sent.
Failed or pending writes cannot produce success. Pending reports cannot be
evicted to make room.

Chat exposes `submit-feedback` on text-channel resources, requiring Comment.
The action validates the channel's workspace, derives attribution from the
invoking envelope and refuses changed payloads or authors reusing an identity.
Concurrent first submissions survive conflicting CRDT root maps; concurrent
retries appear as one feedback message. Immutable report content remains
available separately from the ordinary message. The `triage` report action
requires Write and records attributed status changes, duplicate targets or an
address of an existing task. A task reference grants no access and creates no
external task automatically.

## Host integration still required

The shared composer and native/browser entry points are not mounted yet. A host
must supply its existing durable profile store, an admitted
`Destination { room, provider, channel }`, authenticated action transport,
content storage, and an explicit focused-surface capture adapter. The composer
must show the masked preview and offer description, context/screenshot opt-in,
redaction, approval, durable send/retry status and a link to the resulting report.
Use shared toolkit controls, themed/localized presentation and semantic actions.

The destination receipt adapter must check the **canonical**
`RecordStore::save_status(record)` at the time it reads the canonical document.
Use `chat::feedback::receipt_from_record` with the retained canonical owner,
record id and admitted destination. It checks current durability and room scope
before reading the report. MemoryOnly, pending saves
and failures are not receipts. The UI's cached Saved indicator is not a receipt
witness: it can describe earlier heads. Retain the authenticated source identity
through this adapter; never derive it from report payloads or URLs. The submitting
host feeds the receipt to `Outbox::confirm_durable_receipt` with that identity.
Persist masked screenshot bytes before allowing its immutable payload to send.

This integration uses the existing public demo deployment. It does not authorize
publishing, changing access policy or connecting personal resources/accounts.

## Focused verification

Run on the existing builder:

```sh
rcargo test --locked -p tilo-feedback-review --lib
rcargo test --locked -p tilo-feedback --lib
rcargo test --locked -p chat --lib feedback::tests
rcargo --no-fetch clippy --locked -p tilo-feedback -p chat --all-targets -- -D warnings
rcargo --no-fetch check --locked -p shell --tests
```

Before enabling the journey, exercise the mounted native and browser production
paths with a synthetic report, reviewed opt-in context and masked attachment;
force storage/transport failures and restart; then verify one attributed durable
report, truthful success/retry, triage permissions and the linked existing task.
