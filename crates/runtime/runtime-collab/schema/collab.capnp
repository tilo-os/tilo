@0xc418c22ce00b8d8d;

# qOS collaboration broker — public Cap'n Proto contract.
#
# This is the wire surface app SDK clients talk to. The broker is
# the compositor's in-session broker; when an app is launched
# under qOS, the launcher hands the app a connected Unix socket
# whose other end speaks this protocol.
#
# Streaming caveat (capnp-rpc 0.26): there is no first-class
# server-push streaming primitive. We model "stream" as a long-lived
# capability the client polls with `read @0`. Each `read` returns a
# single event; the server side blocks the `read` promise until a
# value is available; the client invokes `read` again in a loop.
# This is also how capnproto's own examples (`calculator.capnp`)
# model unbounded fan-out. Back-pressure is implicit (no `read`
# outstanding = no delivery).
#
# ## Schema evolution rules
#
# All method / field ordinals below are the wire contract. When
# adding a new method / field, ONLY append a new ordinal. Never
# reorder or reassign an existing one — doing so silently breaks
# any client compiled against an older schema.

interface CollabService {
  # Join (or create-if-absent) a channel by id, signing the join as
  # `identity`. The wire `identity` is opaque bytes; the broker
  # interprets it as a collab-types serialised identity record
  # (postcard-encoded). We pass bytes rather than a typed struct
  # because the identity format is collab-types' domain and we
  # don't want this schema to drift every time that crate adds a
  # field.
  joinChannel @0 (channelId :Text, identity :Data) -> (handle :ChannelHandle);
}

interface ChannelHandle {
  # Submit a single signed op. `op` is a collab-types SignedOp
  # (postcard-encoded). The broker validates, fan-outs to other
  # subscribers, and persists.
  send @0 (op :Data) -> ();

  # Open a subscription. Returned `Stream` capability yields signed
  # ops one per `read` call, each annotated with the broker's view
  # of who authored it.
  subscribe @1 () -> (stream :Stream);

  # Open the presence channel for this membership. Returned handle
  # owns cursor / caret / selection / arrived / left publish + a
  # unified subscription that carries the full presence payload
  # union (cursor was the only variant in schema v1).
  presence @2 () -> (handle :PresenceHandle);

  # LEGACY — unsigned full-document snapshot. Present so apps that
  # have not yet been provisioned with a signing identity keep
  # working across the ui-collab → runtime-collab migration.
  # Callers on the migration target path use `send` with a
  # collab-types SignedOp instead. Will be removed once identity
  # provisioning covers every fleet app.
  sendUnsignedSnapshot @3 (data :Data) -> ();

  # LEGACY — unsigned incremental Automerge change blob. Same
  # transitional contract as `sendUnsignedSnapshot`; removal
  # deferred until the whole fleet has an identity.
  sendUnsignedIncremental @4 (data :Data) -> ();

  # Subscribe to broker roster + permission-list updates. Returned
  # stream yields a fresh `ChannelContext` snapshot every time the
  # broker's view changes (peer joined / left / channel-owner
  # granted a new role). Client-pull like all streams here.
  subscribeContext @5 () -> (stream :ContextStream);

  # Fetch a page of previously-validated signed ops with
  # `seq < beforeSeq` on the join'd channel. Backing the "scroll
  # up to older edits after late-join" UX; see
  # `ScrollbackBatch` for the response shape.
  #
  # LEGACY — this method does not carry an artifact id, so the
  # broker keys the read on a `nil` artifact (`ArtifactId::default()`).
  # Callers that hold a specific artifact should prefer
  # `fetchScrollbackFor @7`. Kept so schema-v1 clients still resolve.
  fetchScrollback @6 (beforeSeq :UInt64, max :UInt32) -> (batch :ScrollbackBatch);

  # Fetch a page of previously-validated signed ops on a specific
  # artifact. `artifactId` is the 16-byte encoding of the artifact
  # uuid; the broker resolves ops with `seq < beforeSeq` on that
  # artifact only. Introduced so scrollback fetches no longer need
  # to fall back to the nil-uuid placeholder the schema-v1 method
  # forced.
  fetchScrollbackFor @7 (artifactId :Data, beforeSeq :UInt64, max :UInt32)
      -> (batch :ScrollbackBatch);

  # Hand the broker content this app has just stored.
  #
  # A document holds a *reference* to its binaries and never the bytes:
  # inlining a photograph made one Automerge change larger than the peer
  # transport would carry, and a dropped change freezes every replica
  # downstream of it. So the reference travels as an op and the content
  # travels here.
  #
  # This method is the only way bytes leave an app. Apps are sandboxed
  # with a private HOME and XDG_CACHE_HOME, deliberately, so there is no
  # directory an app and its shell can both read and there must not be
  # one. Without a content verb on this interface the shell held the
  # reference its own app had just published and none of the content
  # behind it, and every machine in the room — including the one the
  # image was added on — drew a placeholder that never resolved.
  offerBlob @8 (hash :Text, bytes :Data) -> ();

  # Ask for content a document here references and this app lacks.
  #
  # The broker answers from this machine's store when it already holds
  # the content, and otherwise asks the room. Either way the answer
  # arrives on `subscribeBlobs`, never as this call's return: content
  # may take several round trips over a slow link, and an app blocking
  # a frame on it would stall its own compositor.
  wantBlob @9 (hash :Text) -> ();

  # Subscribe to content arriving for this channel — answers to
  # `wantBlob`, and content a peer offered the room while a document
  # here referenced it.
  subscribeBlobs @10 () -> (stream :BlobStream);
}

interface Stream {
  # Pull the next op. Promise resolves when one is available. The
  # caller is responsible for invoking `read` in a loop; the
  # underlying flow-control is implicit (no outstanding `read` =
  # nothing delivered).
  #
  # `fromUserId` is the broker's attribution string for the author
  # (typically the sending identity's `app_id` or, for peers that
  # never sent `Hello`, a synthetic `peer-<idx>` id). Duplicating
  # this field on the RPC rather than folding it into the postcard
  # envelope lets non-Rust clients cheaply route by author without
  # decoding the payload.
  read @0 () -> (op :Data, fromUserId :Text, kind :OpKind);
}

enum OpKind {
  # What `Stream.read`'s `op` bytes actually are. The stream carries
  # three kinds of frame and they are not interchangeable: a delta
  # must be applied incrementally, a whole document must be merged,
  # and a signed envelope must be decoded before either. Without a
  # discriminator a reader can only guess, and the only available
  # guess — "try to decode a signed envelope, drop it if that fails"
  # — silently discards every unsigned frame.
  #
  # `signed` is ordinal 0 so it is also the default: a producer that
  # predates this field means the postcard-encoded `SignedOp` it has
  # always sent.

  signed @0;
  # `op` is a postcard-encoded `SignedOp`.

  unsignedIncremental @1;
  # `op` is a raw Automerge change delta, applied incrementally on
  # top of the document the reader already holds.

  unsignedSnapshot @2;
  # `op` is a whole Automerge document, merged into the reader's.
  # Sent to a peer that has just arrived and has no history for the
  # deltas to apply against.
}

interface PresenceHandle {
  # Publish this user's cursor position into the channel.
  publishCursor @0 (x :Float32, y :Float32) -> ();

  # LEGACY — pre-`subscribeAll` remote-cursor stream. Yields only
  # cursor-payload events, dropping caret / selection / arrived /
  # left. Kept so schema-v1 clients keep compiling; new code uses
  # `subscribeAll` and discriminates on the payload union.
  subscribeRemoteCursors @1 () -> (stream :CursorStream);

  # Publish the local caret position on the artifact identified by
  # `keyBytes`. `keyBytes` is a postcard-encoded `ui::PresenceKey`;
  # the broker treats it as an opaque routing tag, so the schema
  # never has to grow when `ui` adds a new key variant.
  publishCaret @2 (keyBytes :Data, pos :UInt64) -> ();

  # Publish a selection range. `start == end` MAY be used to
  # represent a caret; either publish path is accepted.
  publishSelection @3 (keyBytes :Data, start :UInt64, end :UInt64) -> ();

  # Publish "the local user has arrived at the keyed object" — the
  # broker fans this out so peers can render an entry badge.
  publishArrived @4 (keyBytes :Data) -> ();

  # Publish "the local user has left the keyed object" — the
  # counterpart to `publishArrived`.
  publishLeft @5 (keyBytes :Data) -> ();

  # Subscribe to every kind of remote presence update in one
  # stream. Each event carries the full `PresencePayload` union
  # (see below); clients discriminate on payload variant.
  subscribeAll @6 () -> (stream :PresenceStream);

  # Publish the local pointer at (x, y), in the coordinate space
  # `keyBytes` names.
  #
  # This is `publishCursor` with the one thing `publishCursor` cannot
  # say. A pointer position is meaningless without the space it was
  # measured in: the same (230, 34) is a different place on the
  # desktop, in a file manager and in a word processor. `publishCursor`
  # takes coordinates and nothing else, so the space was dropped at
  # this door and guessed at afterwards — and a guess made once per
  # receiving window drew one person once per window.
  #
  # `keyBytes` is a postcard-encoded `ui::PresenceKey`, as on every
  # other keyed publish here. For a pointer it is expected to be
  # `PresenceKey::Pointer`, whose `PointerSpace` names the space and,
  # for a surface, which surface. The broker keeps the bytes intact
  # end to end and never re-keys them, so exactly one receiver can
  # place the position and the rest can tell that it is not theirs.
  publishPointer @7 (keyBytes :Data, x :Float32, y :Float32) -> ();
}

interface CursorStream {
  # Pull the next remote-cursor update. Returns the user id (as a
  # short string id), the x/y position. Promise resolves when an
  # update is available.
  read @0 () -> (userId :Text, x :Float32, y :Float32);
}

interface PresenceStream {
  # Pull the next presence event across all payload kinds. Promise
  # resolves when an update is available.
  read @0 () -> (event :PresenceEvent);
}

interface ContextStream {
  # Pull the next channel-context snapshot. Promise resolves when
  # the broker's roster / permission view changes.
  read @0 () -> (ctx :ChannelContext);
}

interface BlobStream {
  # Pull the next piece of arrived content. Promise resolves when the
  # broker has content for this channel.
  #
  # `hash` is the blake3 hex the content is addressed by in every store
  # on every machine. A reader MUST verify the bytes against it before
  # storing them: the key *is* the content, so bytes that hash to
  # something else are not the content that was asked for, and taking
  # them on trust would let one peer poison every replica that later
  # reads that hash.
  read @0 () -> (hash :Text, bytes :Data);
}

# Presence update as delivered on `PresenceStream`. `userId` is the
# broker-assigned attribution string (same shape as
# `Stream.read().fromUserId`). `keyBytes` is a postcard-encoded
# `ui::PresenceKey`; the broker treats it as an opaque routing tag.
# `payload` discriminates the variant — cursor / caret / selection
# / arrived / left.
struct PresenceEvent {
  userId    @0 :Text;
  keyBytes  @1 :Data;
  payload   @2 :PresencePayload;
}

# Presence payload union. Mirrors `ui::collaboration::PresencePayload`
# 1:1. Kept as a native capnp union rather than opaque bytes so
# non-Rust broker peers can discriminate without a postcard
# decoder.
struct PresencePayload {
  union {
    cursor :group {
      x @0 :Float32;
      y @1 :Float32;
    }
    caret :group {
      pos @2 :UInt64;
    }
    selection :group {
      start @3 :UInt64;
      end   @4 :UInt64;
    }
    arrived @5 :Void;
    left    @6 :Void;

    # A pointer whose `PresenceEvent.keyBytes` is meaningful — the
    # payload `publishPointer` produces. Distinct from `cursor` so a
    # reader can tell a position that knows its coordinate space from
    # one that does not: `cursor` is `publishCursor`'s keyless legacy
    # shape and a reader must not invent a space for it, whereas this
    # one is only ever emitted alongside the key its publisher chose.
    pointer :group {
      x @7 :Float32;
      y @8 :Float32;
    }
  }
}

# Roster + permission snapshot delivered on `ContextStream`.
# `collaborators` is the full current membership; `permissions` is
# a bag of string tags naming grants held by the *local* identity
# on this channel (the caller). The broker sends a fresh snapshot
# every time its view changes rather than diffing — this bounds
# the client-side state to O(members) and keeps a slow reader from
# drifting.
struct ChannelContext {
  collaborators @0 :List(Collaborator);
  permissions   @1 :List(Text);
}

# A single collaborator in a `ChannelContext`. `color` is packed as
# 0x00RRGGBB (top byte reserved, MUST be zero); consumers unpack
# into their native colour space. This keeps the wire compact
# without committing to a specific colour representation.
struct Collaborator {
  userId      @0 :Text;
  displayName @1 :Text;
  color       @2 :UInt32;
}

# A page of previously-validated signed ops returned by
# `fetchScrollback`. `ops` are postcard-encoded `SignedOp` blobs in
# ascending `seq` order; `earliestSeq` / `latestSeq` bracket the
# page (both inclusive) so clients can request the next page
# without decoding. `done == true` means the broker has no further
# in-memory history in the requested range — clients that want
# older ops can then fall back to the cooperative gossipsub path.
struct ScrollbackBatch {
  ops         @0 :List(Data);
  earliestSeq @1 :UInt64;
  latestSeq   @2 :UInt64;
  done        @3 :Bool;
}
