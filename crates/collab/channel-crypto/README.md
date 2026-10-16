# channel-crypto

Per-channel MLS group encryption for Tilo, with full history for members
who join later.

## What this crate does

Each Tilo channel maps to one MLS group, used for group key agreement: the
group's epoch secrets rotate on every membership change. Ops are sealed under
per-epoch **content keys** derived from those epochs, and a joining member
receives every earlier content key, so it can read the channel's complete
history. The `SignedOp` envelope's metadata (author, artifact, op class,
timestamp, seq, plus the versioned `EncryptionMetadata`) stays cleartext so
the broker can route by artifact and filter by op class without decrypting.

```rust
use channel_crypto::{ChannelGroup, MemberKey};
use collab_types::{ChannelId, ContentRetention};

// Alice creates a fresh one-member channel.
let cid = ChannelId::new();
let mut alice = ChannelGroup::create(cid, MemberKey::generate("alice")?)?;

// Bob mints a KeyPackage and ships it to Alice out-of-band.
let bob_key = MemberKey::generate("bob")?;
let addition = alice.admit(bob_key.fresh_key_package()?)?;
// `addition.welcome` and `addition.key_history` go to Bob,
// `addition.commit` (from the second join on) to existing members.
let mut bob = ChannelGroup::process_welcome_with_history(
    cid, &addition.welcome, &addition.key_history, bob_key)?;

// Seal and open op payloads under the channel content key.
let (metadata, ciphertext) =
    alice.seal_content(artifact, ContentRetention::Delta, b"hello bob")?;
let plaintext = bob.open_content(artifact, &metadata, &ciphertext)?;
```

Removal returns the commit every surviving member must process:

```rust
let removal_commit = alice.remove_member(bob_leaf_index)?;
carol.process_commit(&removal_commit)?;
```

The broker must durably fan out that commit before it reports removal as
complete. A removed member cannot process the removal commit, derive the next
content key, or encrypt at the new epoch.

## History for members who join later

A member invited later gets access to all of the channel's state and
history.

- **Content keys.** Every MLS epoch `e` has one 256-bit content key,
  `MLS-Exporter("tilo channel content key v2", channel_id, 32)`, derived
  independently by every member of that epoch. Ops are sealed with a
  `tilo-seal` XChaCha20-Poly1305 envelope under the key of the sender's
  current epoch, bound to the channel, epoch and artifact. A content-key
  ciphertext can be opened any number of times by anyone holding the key,
  unlike an MLS application message, which each receiver can decrypt once and
  only at its epoch.
- **Key history on join.** Each member keeps a `ContentKeyring` of every key
  it has: the epochs it lived through plus the ones it was handed. Admitting
  a member seals the adder's whole keyring (including the new epoch's key)
  into a `KeyHistory` under a transport key exported from the joiner's first
  epoch. It travels next to the Welcome in the signed governance frame
  (`MlsGovernanceAction::AddWithKeyHistory`). Its size grows with the number of
  membership epochs, about 40 bytes each, never with the amount of content.
  The joiner can then decrypt everything locally, with no other member online.
- **Current state first, history on demand.** A joiner loads the latest
  snapshot, which is sealed like any other op and retained by hosts as a
  checkpoint through the clear `ContentRetention` class in the metadata.
  Older ops stay with the broker's durable history and the relay, and are
  paged in newest-first through the existing scrollback request.
  `collab_types::history::HistoryPager` issues pages as a view nears its
  oldest loaded op, or until an older version or search hit is reached.
- **Granularity: per channel.** A channel is one document route (app,
  workspace room, record), so per-channel keys are already per-resource. A
  finer split would add key-management state without narrowing who can read
  what, because channel membership is the access boundary. Membership is
  decided per record: a Read Deny on a record, its app or any pattern over it
  keeps the denied devices out of that record's channel alone
  ([permissions.md](../../../docs/architecture/permissions.md) §5.3).

### Security properties

- **Joiners read everything.** This is intended: a member added at epoch `e`
  can decrypt all content-key ops of every earlier epoch.
- **Removal protects future content.** The epoch after a removal commit has
  an exporter secret the removed member never learns, so it cannot derive that
  epoch's content key or any later one. It keeps what it legitimately held
  before.
- **Forward secrecy applies to membership removal, not to history.** Content
  keys are long-lived and stored in every member's sealed state. Compromising
  a current member reveals the whole history, as compromising its document
  replica would. MLS keeps its own forward secrecy and post-compromise
  security for handshake and epoch secrets.
- **No sender authentication inside the envelope.** Any key holder can seal
  for that epoch. Authorship comes from the outer `SignedOp` signature and
  the broker's live role check.
- **Stale epochs are refused.** Once a membership change is journaled, the
  broker (`GovernanceJournal::accepts_document_encryption`) refuses new
  content sealed under an older epoch. A removed member therefore cannot
  inject content under keys it still holds. A lagging member must also catch
  up first; until then, what it seals remains readable by the removed member.
- **Integrity of retention hints.** The retention class is signed with the
  op. Endpoints reject an op whose decrypted frame does not match its class.

### Compatibility

- `EncryptionMetadata` is versioned by `EncryptionScheme`. Its wire tag `1`
  (MLS application message) keeps the exact bytes and signing bytes of every
  earlier op, so version-1 ciphertexts still decrypt for members at that
  epoch. Tag `2` is the content-key scheme.
- Sealed member state is a `tilo-seal` envelope that also carries the
  keyring; any other state file is refused.
- `AddWithKeyHistory` is a new trailing variant, so journals written before
  it still decode.

## Stewards, members and endpoints

Each channel has exactly one **steward**: the device that created its MLS
group. Only the steward issues governance frames, so frames form one hash
chain with no forks to reconcile.

- [`ChannelSteward`] owns the group, the governance journal and a small
  ledger. `reconcile(allowed, requests)` moves the group to the device set
  the caller's access policy allows. It removes devices that lost access
  first, then admits allowed devices that sent an access request. A member
  that sends a newer request because it lost its state is removed and
  admitted again. `outbox()` lists, for each device with an unacknowledged
  delivery, the frames after the last one it acknowledged, and
  `acknowledge` applies a device's cumulative acknowledgement. Each change
  stages the commit, writes the group, signs the frame, writes the ledger,
  appends the frame, then merges; a crash at any point is finished or rolled
  back on the next open. Its records live in a [`StewardStore`]: files on a
  native device (`FileStewardStore`), or memory a browser exports to
  `IndexedDB` (`MemoryStewardStore`). It signs through a [`StewardSigner`],
  which may answer later, as a browser's `WebCrypto` key does.
- [`ChannelMember`] is every other device's view. It starts `Waiting` and
  signs `MlsAccessRequestV1`s that carry fresh key packages. Its MLS
  credential is the device's certified key, and the steward refuses a
  package whose credential names another device. It applies frames once and
  in chain order, whatever order and however often the transport delivers
  them: out-of-order frames are held back, duplicates are ignored, and a
  frame replayed after a crash is recognised by its epoch. It keeps a mirror
  of the journal that carries the plaintext gate and the head epoch for the
  device's broker. A frame its MLS state cannot apply makes it `Waiting`
  again instead of failing.
- [`ChannelContentKeys`] is what an application process receives: the
  current epoch and the content keyring. It has no MLS signer, HPKE or
  ratchet secrets, so an app can seal and open content but cannot change
  membership.

A channel's first frame is `Genesis`. It closes the plaintext gate for
good, whether the channel is new or is being migrated from plaintext.

### Per-edit cost

`examples/edit_overhead.rs` measures sealing on the sender and opening on a
receiver through `ChannelContentKeys` (release build, one x86-64 core):

| payload | added bytes | seal | open |
|--------:|------------:|-----:|-----:|
| 64 B    | 56          | 0.8 µs | 0.7 µs |
| 1 KiB   | 56          | 1.2 µs | 1.5 µs |
| 4 KiB   | 56          | 2.7 µs | 3.8 µs |
| 64 KiB  | 56          | 33 µs  | 52 µs  |

A signed op carrying a 64-byte edit grows from 201 to 276 bytes (75 bytes,
including the encryption metadata).

## Production

End-to-end encryption is on for every collaboration channel, with no
exceptions: there is no plaintext mode to opt into and no server-side
plaintext path.

### How a channel is provisioned

- **Creation.** The device that governs a route's room stewards the
  route's channel (`shell/channel_encryption.rs`). It creates the MLS group
  and its `Genesis` frame the first time an allowed endpoint opens the
  route. `Genesis` closes the plaintext gate for good.
- **Migration.** A channel that carried plaintext before migrates the same
  way on its next open. New content is encrypted from then on. Old
  plaintext stays in the replicas of the members that hold it, and the local
  broker's history pages still serve it to current members, because the
  broker admitted it before the gate closed. Live plaintext on an encrypted
  route is refused as a downgrade.
- **Access policy** (`shell/channel_policy.rs`). A channel is readable by:
  - every person on the room's roster, or named by a grant (a share, a
    time-limited reader), who may Read the room node now, through each of
    their certified devices (root-signed, unexpired, not revoked, admitted
    by the room's governor);
  - the governing identity's own fleet;
  - live service members (below).

  Always-on host certificates are never a person's reading device. The
  steward reconciles the group with this set about once a second and on
  every new request. Invites, removals, role changes that end reading, and
  device enrolment or revocation all become commits.
- **Governance transport.** Frames, access requests and acknowledgements
  travel on the room's topic (`NeighbourMsg::MlsGovernance`,
  `MlsAccessRequest`, `MlsGovernanceAck`). Workspace hosts retain them, so a
  device that is offline still gets its Welcome. The steward redelivers each
  device's unacknowledged frames with exponential backoff (1 s up to 60 s)
  until that device's cumulative acknowledgement covers them. Redelivery is
  idempotent and ordered on the receiving side, so transport failures delay
  delivery and never lose or reorder it.
- **Endpoints.** An app asks the workspace identity service for its route's
  keys (`Request::ChannelKeys`) and gets `ChannelContentKeys`: content
  keys, and no MLS secret. It does not ask again on a timer: each route
  keeps one `channelKeysChanged` call pending at the broker, which resolves
  the moment the channel service records a change to that channel (a commit
  the steward made, or a frame that reached this member), and the endpoint
  asks for keys then. Every policy change above becomes such a commit
  within the steward's reconcile interval, so a removed or revoked device's
  apps stop holding the channel's keys within about a second plus the
  commit's delivery to that device; the broker's epoch gate and the identity
  service's per-signature trust check stop it publishing at once. A route
  also asks when ciphertext arrives that its keys can't open, and when the
  broker refuses an edit. A route still waiting for access is asked on a
  timer as well (1 s, backing off to 30 s; admission itself is pushed), and
  a broker without `channelKeysChanged` gets the old one-second refresh of
  every route. The identity service serves its trust check from the
  identity files' stamps while they are unchanged. While the device waits
  for access, the SDK holds edits and incoming ciphertext on the device and
  reports `ChannelAccessState::WaitingForAccess` through
  `CollabEvent::ChannelAccess` and `App::channel_access()`. Once keys arrive
  it seals, sends and opens them in order. A send refused after a membership
  change is re-sealed under the new epoch and retried.
- **Committed routes** stay cleartext. When the broker serves a route as a
  committed document, its owner (the shell's native workspace owner)
  admits and applies each edit itself, and
  it holds no channel keys: it could not open a sealed edit, and after one
  it refuses cleartext on the route as a downgrade. Endpoints therefore do
  not ask for keys on committed routes, and a host-signed endpoint commits
  through its host. Committed routes take encryption once their owner runs
  as a service member.
- **Rooms a node governs** have no committed routes. A node is a relay whose
  certificate lets it host and own workspaces; it stewards the channels of
  the rooms it governs. A desktop session enrolled in such a room serves
  each route as a channel: its apps ask for keys, seal and open, and its
  sealed writes go to the relay as the device's own, never through the
  governor. A company node's own session is such a member: a device of the
  node's person with its own key and certificate, which joins every room
  the node governs (`TILO_SHELL_FOLLOW_HOST_ROOMS`).
- **Brokers and relays** hold gates, never keys. Every ingress path checks
  the channel's gate (`ChannelGate`): plaintext is closed off, and content
  sealed before the latest membership change is refused. Relays store and
  forward ciphertext and MLS control material without decoding either.

### Service members

A cloud agent run, server-side search or a preview renderer reads a room's
content only as a **service member** (`shell/service_members.rs`):

- It has its own device key, which is also its MLS leaf.
- A room administrator admits it with a purpose, a scope (the verb it may
  exercise) and an optional deadline. The grant is a room-membership
  capability on the agent principal `service:<device key hex>`.
- It is listed in the Agents pane as a service row, with its purpose and
  when access ends.
- When the deadline passes, or `revoke_service_member` ends the job, the
  steward removes it. The removal is a commit, so content keys rotate: the
  service keeps what it read and cannot open anything written afterwards.

The relay and the action broker need no change: they route and retain
ciphertext and MLS control material without decoding either.

### Security properties as shipped

- **Confidentiality from infrastructure.** Relays, workspace hosts and
  brokers see signed envelope metadata (author, artifact, op class, retention
  class, channel, epoch, seq, timestamp) and ciphertext. They never see
  content and never hold a content key. An end-to-end test asserts that no
  byte the relay stores contains the plaintext.
- **Membership follows the room ACL and the certified device set.** No
  separate channel membership list can drift from permissions.
- **Joiners read all history** through the key history in their Welcome.
- **Removal protects future content.** Removing a person, a device or a
  service is an MLS commit; the next epoch's keys are unknown to the removed
  party, and every gate refuses content sealed under older epochs.
- **Apps never hold MLS secrets.** They receive content keys for routes of
  their own launch only, gated on the device being trusted and the person
  holding a reading role.
- **One issuer per channel.** Only the steward signs frames, and members
  refuse frames from anyone else, so governance cannot fork.
- **Forward secrecy applies to membership, not to history**, as described
  above.
- **Known limits.** Stewardship follows room governance: if the governing
  machine is lost, the room's channels keep working for their current
  members, but membership cannot change until governance is re-established,
  exactly like the room itself. Only Welcome-based joins exist; there are no
  external commits.

### Key loss and identity backups

A `tilo-seal` identity backup (`runtime-identity::backup`) holds the root
identity record. It does not hold device keys, device certificates, the
fleet, the Secret Service master key or any MLS state. Restoring it on a
replacement machine brings back the person's root identity, and onboarding
certifies a **new** device key for that machine. Channel access comes back
through the normal paths:

1. The new device announces its root-signed certificate. Once the room's
   governor has admitted it, the policy allows it.
2. The steward admits it, and its key history opens every earlier content
   key, so the whole channel history reads again
   (`a_restored_identity_regains_the_whole_channel_on_its_new_device`).
3. Revoking the lost device (fleet revocation) makes the next reconcile
   remove it, so it cannot read anything written afterwards.

If the lost machine was the room's governor, see the known limit above.

## Remaining work

- **External commit / external join.** Only Welcome-based joins are
  supported.
- **Below-record reads.** Admission is per record: a steward learns which
  record a channel carries from a grant naming it or from the route a device
  proves with its access request (`NeighbourMsg::MlsRoutedAccessRequest`), and
  admits a device, or a service member, only while the same read decision
  allows that record; a channel whose record it cannot name is withheld from
  whoever a Deny over a pattern of records could keep out. A Deny below a
  record (an item, a field) still binds only honest clients and doors: one key
  opens the whole record, so against a modified client the record is the
  confidentiality boundary
  ([permissions.md](../../../docs/architecture/permissions.md) §5.3).
- **Multiple ciphersuites / negotiation.** There is one fixed suite.

## Cipher suite

`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`, the canonical small MLS
suite. Ed25519 matches the rest of Tilo (identity, FROST, Iroh). A single
fixed suite rules out downgrade attacks. A second suite would arrive under a
versioned handshake.

## Implementation notes

- **openmls 0.9** is the pinned line, together with `openmls_rust_crypto`
  0.6, `openmls_basic_credential` 0.6 and `openmls_traits` 0.6.
- **`debug_assert!(false, ...)` on AEAD-tag mismatch.** openmls panics in
  debug builds on an AEAD failure while processing a message. `decrypt` and
  `process_commit` wrap the call in `std::panic::catch_unwind` so a malformed
  peer message never crashes the host.
- **Ratchet tree must be inlined.** Groups use
  `use_ratchet_tree_extension(true)`, so a Welcome is self-contained.
- **Symmetric sealing goes through `tilo-seal`.** Content envelopes, key
  histories and persisted state never use a raw AEAD crate directly.

## Building

```bash
cargo test --locked -p channel-crypto
```
