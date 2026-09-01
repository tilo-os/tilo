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
  joinChannel @0 (channelId :Text, identity :Data, participantHandle :Text) -> (handle :ChannelHandle);
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

  # Retired unsigned verbs keep their ordinals because Cap'n Proto requires
  # declarations to remain sequential. They carry no data and have no effect.
  retiredUnsignedSnapshot @3 () -> ();
  retiredUnsignedIncremental @4 () -> ();

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
  # Retired artifact-less scrollback ordinal. All reads name an artifact via
  # `fetchScrollbackFor @7`; the ordinal remains reserved permanently.
  retiredFetchScrollback @6 () -> ();

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
  # A document holds a reference to its binaries and never the bytes:
  # one photograph inlined into an Automerge change exceeded what the
  # peer transport carries, and a dropped change freezes every replica
  # downstream of it. So the reference travels as an op and the content
  # travels here.
  #
  # This is also the only way bytes leave an app. Each app is sandboxed
  # with a private HOME and XDG_CACHE_HOME, so there is no directory an
  # app and its shell can both read, and there must not be one.
  offerBlob @8 (hash :Text, bytes :Data) -> ();

  # Ask for content a document here references and this app lacks.
  #
  # Answered from this machine's store, or from the room. Either way the
  # answer arrives on `subscribeBlobs`, never as this call's return: a
  # transfer can take several round trips, and an app blocking a frame
  # on one would stall its own compositor.
  wantBlob @9 (hash :Text) -> ();

  # Subscribe to content arriving for this channel — answers to
  # `wantBlob`, and content a peer offered while a document here
  # referenced it.
  subscribeBlobs @10 () -> (stream :BlobStream);

  # Publish a typed action into the room, for whichever app owns the
  # document it belongs to.
  #
  # Every other verb here carries Automerge bytes, which an app can only
  # produce for a document it holds. This one carries a `typeId` naming
  # the owning app and an opaque payload that app's registry decodes —
  # so an app can address another app's document without holding a
  # replica of it, knowing its schema, or being permitted to write it.
  #
  # Fanned out to every connection in the room; a receiver applies it
  # only if its own registry knows `typeId`. Room-wide delivery is the
  # property, not a compromise: narrowing it would put instructions that
  # change shared documents somewhere the people sharing them cannot
  # read.
  sendAction @11 (typeId :Text, payload :Data) -> ();

  # Subscribe to actions other apps in this room published.
  subscribeActions @12 () -> (stream :ActionStream);

  # Tell this session what this application currently has open, and what
  # may be done to it.
  #
  # Addressed to the session rather than to the room: what an app has on
  # screen is a fact about this machine, and the thing that has to know
  # it is the compositor drawing the other windows. An app already
  # describes its open resources to its own command palette, but nothing
  # outside the process could read that description — so a participant
  # that was not the app (another window, an agent, a script) could open
  # an application and not touch what was inside it.
  #
  # `appId` is the app's claim about itself; the session pairs it with
  # the connection that made it and gates on the pairing, not the claim.
  #
  # What crosses is inert. An `ExportedAction` carries ids and labels and
  # no way to fire anything — the closure an `ActionSpec` holds never
  # leaves the process that built it. That is this struct's shape, not a
  # promise: there is nothing here to invoke.
  #
  # Level-triggered, not a delta: each call replaces this connection's
  # whole published surface, so a resource that has closed stops being
  # addressable rather than lingering as an address that resolves to
  # something gone.
  publishActions @13 (appId :Text, actions :List(ExportedAction)) -> ();

  # Subscribe to invocations the session has addressed to THIS
  # application.
  #
  # Unlike `subscribeActions`, which any app in the room may read, this
  # carries only what was addressed to this connection — delivering an
  # instruction meant for one application to the room would let any app
  # in it act on another's behalf.
  #
  # The session performs its capability check before anything reaches
  # this stream, which is also why an app must not treat arrival as
  # authority for anything beyond the operation named.
  subscribeInvocations @14 () -> (stream :InvocationStream);

  # Report what became of an invocation. `id` echoes
  # `InvocationStream.read`'s.
  #
  # An empty `code` means it ran; otherwise `code` is one of
  # `unknown_action` / `invalid_arguments` / `action_failed` and
  # `message` says why in the app's own words. Required rather than
  # optional: the caller asked a machine to do something it cannot see
  # the result of, so silence and success must not look alike to it.
  completeInvocation @15 (id :UInt64, code :Text, message :Text) -> ();

  # Ask this session to open the "say something about this" surface.
  #
  # Addressed to the compositor on the other end of the socket rather
  # than to the room, and carried on this interface because this is the
  # socket every app already holds and what it carries is a resource
  # address — this interface's subject.
  #
  # `reference` is a serialised `qos_reference::ResourceRef` (JSON, the
  # encoding `sendAction` payloads use). `x`/`y`/`w`/`h` are the rect to
  # sit beside, in the ASKING WINDOW's own coordinates: the app reports
  # where its context menu is and the session converts, because only the
  # session knows where that window sits on a desktop.
  #
  # Never relayed to peers. A remark composed here is delivered as an
  # ordinary action once it is written; asking for the panel is not.
  composeRemark @16 (reference :Text, x :Float32, y :Float32,
                     w :Float32, h :Float32) -> ();
}

# One operation an application exports, as inert data.
#
# Mirrors `qos_actions::door::ActionDescriptor`. A native capnp struct
# rather than opaque bytes for the reason `PresencePayload` is: this is
# what a non-Rust participant reads to find out what a window can do,
# and it should not need a postcard decoder to do it.
struct ExportedAction {
  # The app's own id for the thing being acted on
  # (`word-processor.document`, `files.entry:/var/workspace/plan.md`).
  # Never a screen position and never an index into a rendered list: the
  # same address names the same operation across layouts, themes and
  # resolutions, which is what makes this drivable by something that
  # cannot see.
  resource      @0 :Text;
  action        @1 :Text;
  # The resource's stable kind tag, shared with the OS-wide type
  # registry so the same kind means the same thing across apps.
  resourceKind  @2 :Text;
  # Human labels, in the app's own words.
  resourceLabel @3 :Text;
  actionLabel   @4 :Text;
  # Empty when the app supplied none — absent and empty are the same
  # fact here, so there is no separate flag.
  description   @5 :Text;
  input         @6 :ActionInput;
}

# What one exported action's invocation must supply.
#
# Advertised rather than discovered by trying: enumeration is the only
# thing a caller that cannot see the screen has to work from, and an
# argument nobody described could only be guessed at.
struct ActionInput {
  union {
    none @0 :Void;
    text :group {
      prompt      @1 :Text;
      placeholder @2 :Text;
      # Whether that text is the content hash of bytes already in this
      # machine's store rather than words somebody says. Words can be
      # quoted out of a request and a content hash cannot — it exists
      # only because some earlier operation produced the content.
      wantsContent @3 :Bool;
    }
  }
}

interface InvocationStream {
  # Pull the next invocation addressed to this application.
  #
  # `argument` is meaningful only when `hasArgument`; the two are
  # separate so an action that legitimately takes the empty string can
  # still say it was given one.
  #
  # `actor` is who asked, as the session resolved them. The app commits
  # the resulting change under it, so an edit made through this door is
  # attributed in the document's history exactly like one somebody
  # typed. An attribution, not an authority — the session has already
  # decided this invocation may happen.
  read @0 () -> (id :UInt64, resource :Text, action :Text,
                 argument :Text, hasArgument :Bool, actor :Text);
}

interface ActionStream {
  # Pull the next action published into this room.
  #
  # `fromUserId` is the broker's attribution for the sender, in the shape
  # `Stream.read` uses. It is who *sent* the frame, which is a different
  # claim from who the action says authored it — only the first is one
  # the broker stands behind.
  read @0 () -> (typeId :Text, payload :Data, fromUserId :Text);
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
  # Every live stream frame is a postcard-encoded SignedOp.
  signed @0;
}

interface PresenceHandle {
  # Publish this user's cursor position into the channel.
  publishCursor @0 (x :Float32, y :Float32) -> ();

  # Retired cursor-only subscription ordinal. `subscribeAll @6` is the sole
  # presence stream; this ordinal remains reserved permanently.
  retiredRemoteCursors @1 () -> ();

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
  # `publishCursor` with the one thing `publishCursor` cannot say. A
  # position is meaningless without the space it was measured in: the
  # same (230, 34) is a different place on the desktop, in a file
  # manager and in a word processor. `publishCursor` takes coordinates
  # and nothing else, so the space was dropped at this door and guessed
  # at afterwards — a guess made once per receiving window, which drew
  # one person once per window.
  #
  # `keyBytes` is a postcard-encoded `ui::PresenceKey`, expected to be
  # `PresenceKey::Pointer`, whose `PointerSpace` names the space and,
  # for a surface, which surface. The broker keeps the bytes intact end
  # to end and never re-keys them, so exactly one receiver can place the
  # position and the rest can tell that it is not theirs.
  publishPointer @7 (keyBytes :Data, x :Float32, y :Float32) -> ();

  # Publish a transient "just typed" range. This is intentionally distinct
  # from selection: receivers render it as a fading insertion highlight,
  # while a selection is durable presence until the next caret update.
  publishTyping @8 (keyBytes :Data, start :UInt64, end :UInt64) -> ();
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
  # something else are not what was asked for, and taking them on trust
  # would let one peer poison every replica that later reads that hash.
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

    # A pointer whose `PresenceEvent.keyBytes` is meaningful — what
    # `publishPointer` produces. Distinct from `cursor` so a reader can
    # tell a position that knows its coordinate space from one that does
    # not: `cursor` is `publishCursor`'s keyless legacy shape and a
    # reader must not invent a space for it, whereas this one is only
    # ever emitted alongside the key its publisher chose.
    pointer :group {
      x @7 :Float32;
      y @8 :Float32;
    }

    # A transient insertion range. Kept as its own union arm so the local
    # Cap'n Proto hop preserves the same semantic variant as peer transport.
    typing :group {
      start @9 :UInt64;
      end  @10 :UInt64;
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
  # Whether this member is an agent rather than a person. The room knows
  # when it admits one; without carrying it here every consumer had to
  # guess from the id, which is the inference marking exists to avoid.
  # Additive, so a peer that predates it decodes as a person.
  isAgent     @3 :Bool;
  # Whether this row is the receiving machine's own participant.
  #
  # A roster is the room's list of its members, so "which of these is me"
  # is a fact about that list and belongs in it. An application cannot
  # derive it: `userId` is the address *other* machines use for a
  # participant, which a machine never announces about itself, so the
  # only local identity available was a different string for the same
  # person.
  #
  # Additive, so a peer that predates it decodes as somebody else —
  # which is what every row was before this field existed.
  isSelf      @4 :Bool;
}

# A page of previously-validated signed ops returned by
# `fetchScrollback`. `ops` are postcard-encoded `SignedOp` blobs in
# ascending `seq` order; `earliestSeq` / `latestSeq` bracket the
# page (both inclusive) so clients can request the next page
# without decoding. `done == true` means the broker has no further
# in-memory history in the requested range — clients that want
# older ops can then fall back to the cooperative Iroh gossip path.
struct ScrollbackBatch {
  ops         @0 :List(Data);
  earliestSeq @1 :UInt64;
  latestSeq   @2 :UInt64;
  done        @3 :Bool;
}
