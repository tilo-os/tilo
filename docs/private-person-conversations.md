# Private person conversations

Chat exposes certified people as conversation resources. Open, Read and Send use
registered semantic actions, so automation uses the same owner-authorized path
as the visible conversation. A person's cursor also opens a compact composer;
Expand transfers its unsent editor into Chat without sending a message.

## Privacy and authority

Messages belong to the governing host's private pair store, not the shared room
document or public room log. Send's semantic invocation and its body are still
visible to the trusted host. The transport authenticates
the device and encrypts traffic in transit. **This is trusted-host privacy, not
end-to-end encryption against the host:** the local owner or relay stores and
reads message plaintext. Task execution and model requests do not acquire access
to a person's conversation.

Every operation checks the original person's certificate, current room
membership, exact pair and original invocation. A Chat worker cannot supply a
new sender label or reuse another worker's selected view. Reads and live updates
recheck authority; replacing an identity, losing membership or closing the
conversation releases its subscription and cached private bodies. An unavailable
owner is an error, never a fallback to the public replica.

Send is idempotent for its exact invocation and body. The owner persists the
message before acknowledging it; only a matching acknowledgement clears the
submitted editor revision. A failed or late response cannot erase newer typing.

## Moving an unsent editor

Expand uses a bounded, short-lived, single-recipient handoff. The public action
carries only an opaque token. Text, selection and input-method composition travel
through the private authenticated channel and never become action arguments or
room history. Chat acknowledges adoption only after opening the exact person
conversation and safely accepting the editor. Existing unsent content is not
silently overwritten. Ordinary agent/task conversations keep their existing
execution and confirmation behavior, including the existing
[model dispatch cancellation limits](task-model-dispatch-safety.md).
