# Private Messages: WhatsApp acceptance

This is an acceptance procedure, not a completed qualification receipt. The
production host starts the pinned WhatsApp SDK after protected review. Existing
tests do **not** establish successful authenticated pairing, receive, reply or
reconnect through the default app. Private WhatsApp group membership inspection
is available per account through
the normal Messages loan and registered actions. Group messaging and membership
mutations are not implemented; the text codec accepts direct conversations only.
Media and calls are outside the first text-client delivery.

## Installed cohort and consent

The release owner must provide one accepted, signed installation cohort containing
the Messages app and shell, with source commit, package versions, catalogue root
and successful source CI. Do not substitute fixture signatures, copy an unsigned
binary into the session or change publisher namespaces. Signal runtime receipts
are separate and do not qualify WhatsApp.

Before a live run, obtain an explicit handoff naming the test device, the account
owner who will scan the code, the consenting recipient and the bounded test text.
No real account linking or message sending is authorized by this procedure alone.
Use a dedicated private session. Do not invite workspace collaborators. Record
only non-secret status and opaque operation identifiers; never record the pairing
code, credential material or private message contents in screenshots or logs.

## Default enrollment and text journey

1. Start the installed app through the normal launcher. In an authorized VM slot,
   the semantic entry points are:

   ```sh
   tilo-vm control actions invoke applications so.tilo.messages
   tilo-vm control actions list
   ```

   On a desktop use the installed `tilo-control` with the same arguments. Discover
   the Messages provider and action IDs from the returned registry; widget names
   and translated labels are not action IDs. Invoke discovered actions with their
   declared argument schema, using `--args-stdin` for sensitive inputs. Keep the
   phone scan and protected approval in the owner's local interface.

2. With no configured accounts, open account setup and select WhatsApp. Review
   private encrypted storage access in the protected Passwords flow. Declining
   must leave no configured account and must not start SDK network traffic.

3. After approval, request the private linking code and review the protected
   network request for `https://web.whatsapp.com`. The owner scans the current
   code in WhatsApp's linked-device interface. A rotated, expired or canceled code
   must not complete enrollment. Showing a code is not authenticated readiness.

4. Complete the exact current attempt. Review the self identity returned by the
   authenticated SDK, then confirm it. The account must become available only
   after that review; a connection indicator alone is insufficient.

5. Select the recipient from shared Contacts. Review the exact WhatsApp identity
   association and select the explicit sender account. Keep Signal, SMS and Tilo
   routes separate even when they belong to the same person.

6. Have the consenting recipient send the agreed text. Confirm it appears in the
   selected private conversation. Reply through the discovered `Send` action or
   the normal composer. Confirm receipt on the other endpoint. A local bubble or
   SDK submission reference does not prove remote delivery; correlate the actual
   receipt without logging text.

7. Close and reopen Messages and reconnect the same enrolled identity. Verify
   encrypted history reopens under that exact service/account/recipient and that
   no fresh linking code or duplicate outgoing message appears. Reopen another
   service's history to verify separation.

8. Revoke the account's original protected permission while connected. Subsequent
   reads, sends, reconnects and durable writes must refuse; previously submitted
   operations must not replay. Confirm no private conversation is exposed through
   workspace Chat, collaboration or another account's history.

For an isolated automated peer, use the same SDK, enrollment host and broker
paths. A fake adapter send result or manually set logged-in/connected flag is not
this journey. The upstream zero-signed mock certificate fails the production SDK
certificate gate. Verification-skip features are prohibited. A local TLS CA
would address only HTTPS/WebSocket authentication: the separate Noise verifier
uses the SDK-pinned WhatsApp XEdDSA issuer, verifies intermediate and leaf
signatures, and binds the leaf to the handshake server-static public key.

A certificate-valid isolated peer therefore needs a WhatsApp-issued test chain
and its matching server-static private key, plus a TLS certificate accepted by
the existing verifier. Captured certificates alone cannot answer a fresh Noise
handshake. No such accepted fixture artifact is presently available. Do not
create a new issuer, replace a pinned root, install a persistent CA, enable
`danger-skip-*` features, or retry the denied private image pull. Authorization
for a particular existing provider test artifact must name its owner and exact
isolated use; it does not substitute for that artifact. A transparent connection
to the real service remains real-account testing and needs explicit approval for
the named device, account and recipient. No credentials or live messages have
been used for this qualification.

## Private group inspection

Select the explicit WhatsApp account's group entry or discover `ListGroups` and
`ReadGroupMembership` in the action registry. Both are sensitive operations. They
fetch fresh SDK service metadata, require exact authenticated self membership and
recheck original authority and self identity after I/O. PN and LID identities are
not merged by name. Inspect a group's member roles, then refresh after a service
membership change. Revoke the original grant and verify the private panel clears
and another read refuses. These views do not change the workspace document and
do not grant permission to send to a group or alter membership. Controlled host
fixtures qualify projection and refusal behavior; successful authenticated group
service requests remain part of the separately blocked transport qualification.


## Private group text and membership changes

`ReadGroupHistory`, `SendGroupText`, `AddGroupMember`, `RemoveGroupMember` and
`LeaveGroup` are device-only registered actions, backed by the default account
supervisor's real SDK client. Read and send require sensitive review; membership
changes require destructive confirmation. The member view returns an opaque,
short-lived review tied to the exact service/account, authenticated self PN/LID,
roster, roles and group mode. Original protected custody gates every read and
write. Each write fetches fresh service metadata, refuses a changed or expired
review, and atomically consumes that review while journaling the operation before
service I/O. SDK encryption also requests fresh group metadata. Failed or
ambiguous writes remain uncertain and cannot replay on restart.

Adding a member requires a current explicit Contacts association for this sender
account, provider scope and identity revision, plus authenticated administrator
status. Removing a member requires administrator status and exact current member
identity; leaving uses the authenticated self identity. Names, workspace roles,
phone-number aliases and stale Contacts choices confer no authority. Unsupported
community, suspended, incognito or disappearing-message groups refuse before
writing. Invitations requiring extra acceptance are not reported as completed.

Group text is encrypted under the original account custody, separated by
account, group and canonical author/stanza identity. Received direct and group
text share the broker's existing encrypted history transaction; roster reviews
and one-shot sends retain the original encrypted SDK custody. Inbox batches
prevalidate before a single commit; duplicate IDs from different members remain distinct.
Original grant revocation stops every journal, receipt, inbound and history
operation. History is projected only into local state and includes submission or
uncertain status, never manufactured recipient delivery. Successful sends and
member changes refresh the roster review; leaving closes the private group view. Expired reviews can be refreshed without losing a draft.
An already opened history responds to the original supervisor's change signal.

Mixed direct/group offline SDK batches use the same all-or-nothing encrypted
history commit. A conflict, unsupported direct payload, foreign SDK source or
revoked original grant prevents both histories from changing. Redelivery is
idempotent across both namespaces and acknowledges only after durable commit.
A certificate-valid isolated service peer remains unavailable. Focused source
regressions qualify action confirmation, private projection, original-grant
revocation, encrypted persistence, atomicity and no replay. They do not qualify a
positive provider-authenticated pairing or group operation.
