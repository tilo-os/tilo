# Shared Contacts and account identities

Contacts owns the `tilo_contact_directory::ContactDirectory` person record,
kind `tilo.contacts.identities`. It uses the existing `SharedRecord` host:
Contacts is the owner and Messages is the explicit editor; other apps read
through the host's existing person-record boundary. A record follows its owner
rather than workspace collaborators. Capture surfaces never publish fixtures.
No additional filesystem access, account-editor permission or credentials are
introduced. The UI crate split is unaffected.

The directory has stable Contacts UUIDs, labels, phone and email hints, deletion state and
confirmed service endpoints. Labels and hints never authenticate a person.
A Tilo endpoint represents a stable user identity, not a device key. Association
never grants a workspace role, resource access, or cryptographic trust.

`Endpoint { service, scope, recipient, revision, removed }` retains opaque provider
subjects and namespaces. `scope` distinguishes provider namespaces, including
WhatsApp business scopes. `revision` is the provider's identity-key fingerprint
or assignment generation. Signal usernames are initiation handles, not enduring
account ids. SMS adapters need an assignment generation and must report unknown
ownership rather than claiming a recycled number still belongs to the old owner.

Primary references: [Signal's identity/privacy explanation](https://signal.org/blog/phone-number-privacy-usernames/)
and [Meta's business-scoped user IDs](https://developers.facebook.com/documentation/business-messaging/whatsapp/business-scoped-user-ids.md/).
The scoped, revision-aware contract is Tilo's design derived from these identity
constraints, not a claim that all providers expose reassignment evidence.

## Contacts projection and confirmation

The live Contacts surface projects labels, telephone/email hints and deletion
UUIDs with `PROJECT_CONTACT`, `PROJECT_PHONES`, `PROJECT_EMAILS` and `REMOVE_CONTACT`.
`phone_candidates` returns every exact international-number candidate, even
when there is only one. It never creates an endpoint. National numbers, suffix
matching and extensions do not guess an identity. Importing a vCard supplies
hints; exporting it does not disclose internal account associations.

`CONFIRM_ACCOUNT.with((person, association, service, scope, recipient, revision))`
records the user's exact reviewed association. Its resource action requires
confirmation. `FORGET_ENDPOINT.with((person, association))` retains a tombstone;
a delayed confirmation cannot restore or retarget that association id.
Sequential competing associations refuse. Concurrent confirmations remain in
the replicated record, and resolution refuses ambiguity rather than choosing a
winner. A changed revision requires review and a new association id. `REPLACE_ACCOUNT`
atomically tombstones the old association and confirms the new revision; a
refused replacement leaves the old association intact. `REPLACE_ENDPOINT`
provides the same atomic replacement for unversioned address-book entries.

Deletion excludes a person's endpoints; delayed label projection cannot revive
them. Contacts undo/restoration needs a separate reviewed reactivation policy.
Merging contacts does not silently transfer external identities: confirm them
again on the surviving person. Identity proof remains with each provider and
its cryptographic verifier; the directory records an address-book decision.

## Messages and cross-app operation account choice

Consume the same authorized record with `shared_person::<ContactDirectory>()`.
Call `resolve_account(service, scope, recipient, current_revision)` immediately
before constructing a route. An unknown/changed revision or conflicting
association refuses resolution. Store the association id and remote account
provenance with historical messages; linking people never rewrites old messages
or merges service groups. Messages switches histories through its header
selector; Chat remains persistent shared spaces.

The transitional `CONFIRM_ENDPOINT` / `resolve(service, recipient)` API retains
existing Messages source compatibility. It creates unversioned associations.
The old resolver cannot resolve versioned endpoints, and unversioned endpoints
cannot generate transport routes. Messages' revision-aware migration is required
before an adapter uses this directory to send; no compatibility downgrade may
bypass that migration.

`routing::routes(directory, person, service, senders)` returns sender/recipient
pairs retaining the person, association id, provider scope and identity revision.
Senders are projections of existing OS account metadata, not a second account
store. Adapters supply only currently usable accounts and keep credentials in
the existing device vault. `select_route` requires a choice when several pairs
exist; a disconnected or stale explicit choice returns unavailable. It never
consults the OS active/default account. Even a ready route requires current
provider evidence and the existing device grant before I/O.

The same pattern supports unified reads from many accounts while preserving
which account supplied each result and requiring an explicit sending identity.
This change adds no Gmail or Calendar integration and does not modify their
account selection behavior.

## Integration boundaries

Contacts exposes account editing in the redesigned `NavShell` inspector using
shared controls. A contact resource's `OpenAccounts` action opens that inspector;
the shell supplies its header, toggle and responsive placement. The inspector is
attached on that first request so an unused account toggle does not displace the
compact browsing header's commands.
Additions, edits and removals stage a review before mutating the directory.
Pending reviews are `contacts.account-review` resources with a row-scoped,
confirmation-required action. Changing contact, closing, or cancelling invalidates
the review; closing the shell inspector also invalidates it. Changed endpoint
snapshots refuse confirmation. Duplicate names show
a stable UUID suffix in the contact picker.

Manually entered addresses have no identity evidence and cannot produce transport
routes. Editing an address clears its old provider scope/revision. Provider-driven
`contacts.accounts` review actions may supply current scope/revision for explicit
association, but never prove ownership or grant permission. Adapter-supplied
identity evidence remains Messages integration work. The contact form stays
mounted while accounts are edited, preserving unsaved drafts. Messages owns its
confirmation UI, account selector, transports and service histories. This crate
provides no messaging transport, identity proof, credential custody, or automatic
cross-service group mapping. An unavailable shared record stays unavailable.

## Mail address-book selection

Mail reads the same Contacts-owned person record without an editor grant. Its
Contacts recipients header action opens system search scoped to `mail.contacts`.
Every `mail.contact-draft` resource is one explicit person UUID, mailbox hint and
sending-account choice. Duplicate names and addresses remain separate choices;
there is no first-match or active-account fallback. The resource's confirmed
`prepare-contact-draft` action prepares a local draft, never sends it. Agents and
accessibility invoke that same row-bound action. The review description names all
three choices. Changed/deleted contact hints or changed/removed sender accounts
remove the old row and refuse its action; existing drafts are never overwritten.

`email_candidates` returns all matching live person IDs, including a unique
candidate. `canonical_email_hint` accepts conservative ASCII dot-atom mailboxes,
preserves local-part case and folds only the DNS domain, following
[RFC 5321 section 2.4](https://www.rfc-editor.org/rfc/rfc5321#section-2.4). It is
lookup normalization, not deliverability validation or identity proof. Provider
dot/plus aliases, display-name parsing, quoted local parts and internationalized
mailboxes are not guessed; Mail's existing manual entry remains available.

A Contacts draft retains its reviewed sending-account ID and email through
saving/reopening, including the Drafts-folder projection. That projection reuses
one row and reads the canonical draft metadata rather than reconstructing it
from message headers. Sending refuses if the current account differs, rather than
following a later sidebar selection. The outbox delivery also matches the
reviewed account ID and email; two accounts sharing an email cannot substitute
for one another, and removal of the reviewed account refuses delivery. This is an address-book selection, not a
confirmed endpoint, provider identity proof, workspace permission or SMTP grant.
No credentials are stored in the directory or draft. Calendar attendee lookup
and other non-messaging consumers remain to be integrated; provider evidence
and live Messages journeys remain with the Messages task.
