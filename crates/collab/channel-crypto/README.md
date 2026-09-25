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
  what, because channel membership is the access boundary.

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
- Sealed member state is now a `tilo-seal` envelope that also carries the
  keyring. State files from earlier builds (`QMLSG` v1) still load. Their
  keyring starts at the loaded epoch, and the next save writes the current
  format.
- `AddWithKeyHistory` is a new trailing variant, so journals written before
  it still decode.

## Production integration status

The production collaboration transport accepts encrypted document envelopes.
The identity service and both the local and network broker paths validate
the outer signature, the current workspace role and capability, the exact
artifact and attestation, and the deterministic route-derived MLS channel.
They then store and fan out the payload byte for byte without decoding it.
Content-key ciphertext is also appended to the broker's durable history
(`$XDG_STATE_HOME/tilo/collaboration/history`), which serves scrollback pages
past the in-memory window. The relay retains encrypted snapshots and the
changes after them using the signed retention class.

The UI collaboration endpoint can be given a provisioned `ChannelGroup`. Once
it is installed on a route, sends seal before signing, and receives (live and
scrollback) open only after broker validation. Missing or wrong group state,
or a missing content key, drops the message rather than treating ciphertext
as plaintext.

That is transport integration, not automatic key provisioning. The shell's
broker-internal workspace-authority transaction admits and removes members
through the governance journal, recording the joiner's key history with the
Welcome. The authenticated peer control transport still needs to drain and
acknowledge the journal's targeted outbox. Applications must not claim E2E
encryption based only on wire and endpoint support.

## Remaining work

- **Production membership provisioning UX** and the journal outbox drain
  described above.
- **Plaintext-history migration.** Installing an MLS group makes that
  endpoint reject plaintext live and scrollback envelopes for the route.
  Existing plaintext history therefore needs an explicit, authority-approved
  encrypted snapshot migration before MLS is enabled.
- **External commit / external join.** Only Welcome-based joins are supported.
- **Vault recovery ceremony.** Production custody uses the session's
  freedesktop Secret Service and never creates a key beside its ciphertext.
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
rcargo test --locked -p channel-crypto
```
