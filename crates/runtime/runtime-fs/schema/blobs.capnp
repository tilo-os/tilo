@0xc3a91f5e27d4b806;

# Tilo document media — public Cap'n Proto contract.
#
# Images and other binaries a document references live in the device's one
# content store, owned by the filesystem daemon. A document holds only a
# `BlobRef`; the bytes are reached through this capability.
#
# The capability is scoped. The launcher connects to the daemon on the app's
# behalf and names the app and its room in a handoff frame the app never sees,
# so the `DocumentBlobs` an app receives answers only for document media of
# that room: content an app in the room put there on this device, or that a
# collaborator's device in the room served for it. Knowing a hash never grants
# access: anything else (files, other rooms, content that does not exist)
# reads as `unavailable`, so the answer cannot be used to probe the store.
#
# ## Schema evolution rules
#
# Only append ordinals. Never reorder or reassign an existing one.

interface DocumentBlobs {
  # Store `bytes` and answer their reference. The whole payload is hashed
  # before the reply, whether or not the store already held it. `mime` is
  # echoed into the reference; the store keeps bytes, not descriptors.
  # Payloads above `maxPutChunk` must use `upload`.
  put @0 (bytes :Data, mime :Text) -> (ref :BlobRef);

  # Stream a payload too large for one message. Nothing becomes readable
  # until `finish` answers.
  upload @1 (mime :Text) -> (upload :BlobUpload);

  # Read up to `maxLen` bytes of `hash` from `offset`. `pending` means this
  # device is fetching it from a collaborator in the room: ask again later
  # rather than waiting on this call. `unavailable` covers both "does not
  # exist" and "not yours to read".
  get @2 (hash :Text, offset :UInt64, maxLen :UInt32) -> (read :BlobRead);

  # Whether `hash` is readable through this capability and its bytes are on
  # this device now.
  has @3 (hash :Text) -> (present :Bool);

  # The content length of a readable `hash`, if this device knows it.
  size @4 (hash :Text) -> (known :Bool, len :UInt64);

  # Current owner-scoped persistence witness, never a cache/presence check.
  # Unavailable covers both missing bytes and a hash outside this scope.
  # Unqualified backends and older servers cannot attest durability.
  durability @5 (hash :Text) -> (status :DurabilityStatus);
}

enum DurabilityStatus {
  unsupported @0;
  unavailable @1;
  durable @2;
}

interface BlobUpload {
  write @0 (bytes :Data) -> ();
  finish @1 () -> (ref :BlobRef);
}

struct BlobRef {
  # Lowercase hex BLAKE3 of the content.
  hash @0 :Text;
  len @1 :UInt64;
  mime @2 :Text;
}

enum ReadStatus {
  ready @0;
  pending @1;
  unavailable @2;
}

struct BlobRead {
  status @0 :ReadStatus;
  # The requested range, when `status` is `ready`.
  bytes @1 :Data;
  # Length of the whole content, when `status` is `ready`.
  totalLen @2 :UInt64;
}

# Served only to the desktop session itself. The session acts on behalf of an
# app it has authenticated (the shell staging content an app asked it to open
# in another app), so it reads through that app's scope rather than around it.
interface SessionBlobs {
  scope @0 (room :Text, app :Text) -> (blobs :DocumentBlobs);
}
