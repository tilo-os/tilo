@0xc418c22ce00b8d8d;

# Tilo collaboration broker — public Cap'n Proto contract.
#
# This is the wire surface app SDK clients talk to. The broker is
# the compositor's in-session broker; when an app is launched
# under Tilo, the launcher hands the app a connected Unix socket
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
  #
  # `recordKind` names the resource kind of the record the channel carries,
  # empty when the app names none. A host that creates a missing record on
  # the app's first write creates it in this kind, if the app declares it.
  joinChannel @0 (channelId :Text, identity :Data, participantHandle :Text,
                  participantKind :Text, recordKind :Text) -> (handle :ChannelHandle);

  # Whether an app can open documents in the room `channelId` names, on
  # this device, now. `joinChannel` holds a join while the room is waiting,
  # so a client asks first only to say why it waits.
  roomStatus @1 (channelId :Text) -> (status :RoomStatus);

  # The room's status once it is no longer `seen`: ready, refused, or
  # waiting for another reason.
  awaitRoom @2 (channelId :Text, seen :RoomStatus) -> (status :RoomStatus);
}

# Where a room stands for the apps on this device. All empty: ready.
struct RoomStatus {
  # Not ready yet, and why, in the person's words.
  waiting @0 :Text;
  # What last held a waiting room up, when something did.
  detail @1 :Text;
  # It will not be ready on this device, and why, in the person's words.
  refusal @2 :Text;
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

  # `publishActions` as a delta against this connection's current surface,
  # for surfaces whose actions repeat one catalogue across many resources
  # (every page of a notebook offers the same editing actions). Each distinct
  # catalogue entry crosses once as a shape, and each distinct ordered set of
  # shapes once as a template, each named by a content hash the publisher
  # chose. A resource is listed with the fields that are its own and the
  # template of the actions it offers. The broker expands the result into the
  # same published surface `publishActions` installs, so readers never see the
  # difference.
  #
  # `reset` discards what this connection published before applying the
  # rest. Without it the delta applies to the connection's surface as the
  # broker holds it: shapes and templates are added, a listed resource
  # replaces what the broker holds of that resource, and `removed` drops
  # resources. A delta that does not apply (an unknown shape or template, an
  # invalid action) is refused and changes nothing; the publisher then sends
  # a `reset` delta.
  publishActionSurface @61 (appId :Text, reset :Bool,
                            shapes :List(ActionShape),
                            templates :List(ActionTemplate),
                            resources :List(ActionResource),
                            removed :List(Text)) -> ();

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
  # Opt in only when the app carries invocationUuid through dispatch and
  # typed lifecycle updates. Old callers retain the numeric-only path.
  subscribeInvocations @14 (stableInvocationIds :Bool = false) -> (stream :InvocationStream);

  # Report what became of an invocation. `id` echoes
  # `InvocationStream.read`'s.
  #
  # An empty `code` means it ran; otherwise `code` is one of
  # `unknown_action` / `invalid_arguments` / `action_failed` and
  # `message` says why in the app's own words. Required rather than
  # optional: the caller asked a machine to do something it cannot see
  # the result of, so silence and success must not look alike to it.
  completeInvocation @15 (id :UInt64, code :Text, message :Text) -> ();

  # Additive v2 lifecycle update. `updateJson` is a serialized
  # tilo_actions::InvocationUpdate. Legacy clients continue using @15.
  updateInvocation @17 (id :UInt64, updateJson :Text) -> ();
  # Cooperative cancellation flag polled by background application jobs.
  invocationCancellation @18 (id :UInt64) -> (cancelled :Bool);

  # The direct published-action methods below stay live beside
  # remoteAction (@42) for brokers and callers that still use them.
  # Authenticated, session-wide resource/action directory. The broker
  # derives the caller from this joined channel; no actor string crosses
  # the wire. JSON preserves the evolving ActionDescriptor vocabulary.
  actionCatalogue @19 () -> (catalogueJson :Text);
  subscribeActionCatalogue @20 () -> (stream :ActionCatalogueStream);
  invokePublishedAction @21 (peerId :UInt64, resource :Text, action :Text,
                             argument :Text, hasArgument :Bool,
                             confirmationJson :Text)
      -> (id :UInt64, updateJson :Text);
  publishedInvocationStatus @22 (id :UInt64)
      -> (found :Bool, updateJson :Text);
  cancelPublishedInvocation @23 (id :UInt64) -> (updateJson :Text);
  # Optional content-addressed object to stage read-only for the launched app.
  # Empty keeps the ordinary app-launch behavior.
  # Optional exact signed installed package root. Empty selects the normal
  # application; a supplied root must match both its catalogue row and app id.
  launchPublishedApplication @24 (appId :Text, contentHash :Text, packageRoot :Text, onBehalfOf :Data)
      -> (id :UInt64, updateJson :Text);

  # Ask this session to open the "say something about this" surface.
  #
  # Addressed to the compositor on the other end of the socket rather
  # than to the room, and carried on this interface because this is the
  # socket every app already holds and what it carries is a resource
  # address — this interface's subject.
  #
  # `reference` is a serialised `tilo_reference::ResourceRef` (JSON, the
  # encoding `sendAction` payloads use). `x`/`y`/`w`/`h` are the rect to
  # sit beside, in the ASKING WINDOW's own coordinates: the app reports
  # where its context menu is and the session converts, because only the
  # session knows where that window sits on a desktop.
  #
  # Never relayed to peers. A remark composed here is delivered as an
  # ordinary action once it is written; asking for the panel is not.
  composeRemark @16 (reference :Text, x :Float32, y :Float32,
                     w :Float32, h :Float32) -> ();

  # A small, path-free continuation projection for this application's current
  # CRDT state. This is session-local input to the shell's same-identity
  # continuation lane; it is neither relayed to collaborators nor an
  # instruction to restore/focus/play anything.
  reportContinuation @25 (appId :Text, schemaVersion :UInt16, metadata :Data) -> ();

  # Level-triggered counterpart to reportContinuation. Apps send this when
  # their current state no longer has a safe resume projection, so stale
  # private metadata is not retained after serialization or policy changes.
  clearContinuation @26 (appId :Text) -> ();

  # The application's answer to a `ChannelContext.handoff` request, or its
  # report that the document it shows arrived retired. Bound to this
  # certified primary route like reportContinuation, and never relayed to
  # peers. The JSON vocabulary is `ui::collaboration::handoff`.
  handoff @53 (appId :Text, message :Data) -> ();

  # Open a session-local bridge to the shell's existing authenticated surface
  # stream and input firewall. The target is the exact 32-byte certified Iroh
  # endpoint identity. Opening never grants control; the returned stream stays
  # waiting until that target publishes a consented SurfaceStreamStart.
  openWorkspaceControl @27 (targetDevice :Data) -> (handle :WorkspaceControlHandle);

  # A delegated agent subscribes to broker-minted task leases addressed to
  # its admitted live session. Offline agents are never queued here.
  subscribeTaskRuns @28 () -> (stream :TaskRunStream);
  # The agent may report only updates for the exact broker-minted run+lease.
  # `updateJson` is a versioned TaskRunUpdate envelope.
  reportTaskRun @29 (runId :UInt64, lease :Text, updateJson :Text) -> ();
  # Invoke a published action while carrying the leased task's ActionContext.
  # The broker derives both agent identity and context; no caller-supplied
  # attribution/context crosses this method. `invocation` is the caller's
  # tilo_actions::InvocationId: a retry with the same id reports the
  # original invocation, which remoteAction (@42) observes and cancels.
  # `waitMs` (non-zero) on a re-ask of a native task's existing child asks
  # the broker to hold the answer until the child moves past `seenJson` (a
  # tilo_actions::remote::UpdateMark), settles, the run is cancelled, or the
  # wait (capped by the broker) ends; the answer is observed and authorized
  # afresh when it is given. A broker without it answers at once.
  # Preserve the published parameter order. Additional fields append after
  # provider; they must not shift waitMs, seenJson or any earlier wire slot.
  invokeTaskAction @30 (runId :UInt64, lease :Text, peerId :UInt64,
                        resource :Text, action :Text, argument :Text,
                        hasArgument :Bool, confirmationJson :Text,
                        invocation :Text, waitMs :UInt32, seenJson :Text,
                        provider :Text,
                        retiredStepIndex :UInt32, retiredHasStep :Bool)
      -> (id :UInt64, updateJson :Text, retiredContinuation :Data);
  # Cooperative cancellation for a leased task. The broker reflects a Task
  # Center cancellation; it never lets an agent cancel a task by itself.
  taskRunCancelled @31 (runId :UInt64, lease :Text) -> (cancelled :Bool, controlJson :Text);

  # Record a terminal effect started by this application's own local UI.
  # The broker re-resolves resource/action against the connection's current
  # published surface and derives actor/workspace/context itself. Arguments,
  # output and failure prose never cross this durable boundary.
  recordLocalEffect @32 (resource :Text, action :Text, updateJson :Text) -> ();

  # Register a revisioned, pull-only directory for resources which are too
  # large or too dynamic to mirror through `publishActions`. The capability is
  # owned by the application connection; the broker never retains resource
  # bodies or an unbounded copy of its rows. Calling this replaces the prior
  # directory capability for this connection. The legacy eager surface at @13
  # remains available to old applications and small/static surfaces.
  publishActionDirectory @33 (appId :Text, directory :ActionDirectory) -> ();

  # Query the authenticated, broker-filtered session directory. `cursor` is
  # opaque and bound to the source revision and caller-visible query. A stale
  # cursor produces `staleCursor`; callers restart from an empty cursor. The
  # broker clamps every budget before forwarding it to an app.
  queryActionDirectory @34 (query :ActionDirectoryQuery) -> (page :ActionDirectoryPage);

  # Resolve exactly one row immediately before invocation. The broker applies
  # current membership policy and asks the live owner to re-materialize the
  # descriptor at `revision`; an unavailable, changed, or disconnected owner
  # returns a non-ok status and is never invoked from cached catalogue data.
  resolvePublishedAction @35 (peerId :UInt64, revision :Text,
                              resource :Text, action :Text)
      -> (resolution :ActionDirectoryResolution,
          invocationBinding :PublishedInvocationBinding);

  # Level-triggered private media handoff descriptors. The broker binds appId,
  # workspace and process instance to this channel and keeps source locators
  # session-local; only sanitized now-playing metadata enters device sync.
  reportPlayback @36 (appId :Text, reportsJson :Data) -> ();

  # Ask the broker to select exactly one existing holder of this route and
  # request a complete signed snapshot. The request itself is directed and
  # carries no document data; it replaces the old presence-based broadcast
  # where every holder serialized and fanned out the same full document.
  requestDocumentSnapshot @37 () -> ();

  # Versioned, bounded active-session continuity control. The JSON vocabulary
  # is owned by tilo-app's realtime_continuity module. Device routing and app /
  # workspace identity are re-derived by the broker; apps receive only opaque
  # restart-scoped session capabilities.
  realtimeCommand @38 (appId :Text, commandJson :Data) -> (replyJson :Data);
  # Directed lifecycle edges for this exact certified application connection.
  subscribeRealtime @39 () -> (stream :RealtimeStream);

  # Files' local semantic action reaches the shell through the same
  # authenticated application connection as every other system operation.
  # The JSON vocabulary is typed and bounded by runtime-collab; the broker
  # re-derives the caller/app/workspace route and alone may turn an exact path
  # into an existing personal-device transfer. No file bytes, certificate, or
  # ambient device authority crosses this command surface.
  nearbyTransferCommand @40 (appId :Text, commandJson :Data) -> (replyJson :Data);

  # Settings-only, launcher-authenticated access to the shell's sealed
  # personal Wi-Fi credential authority. The command contains portable Wi-Fi
  # semantics only; NetworkManager UUIDs are accepted solely as local lookup
  # capabilities and never enter personal-fabric state.
  wifiContinuityCommand @41 (appId :Text, commandJson :Data) -> (replyJson :Data);

  # One tilo_actions::remote::RemoteActionFrame request, answered by one
  # frame: list the actions offered to this connection, invoke one, observe
  # or cancel an invocation. This is the single caller-side door to
  # published actions, shared with peers on the shell network. The broker
  # derives the caller from this admitted channel and confines it to the
  # channel's room; nothing in the frame names the caller.
  remoteAction @42 (frame :Data) -> (frame :Data);

  # Retired per-recipient task delivery verbs keep their ordinals because
  # Cap'n Proto requires declarations to remain sequential. They carry no
  # data and have no effect.
  retiredTaskDelivery @43 () -> ();
  retiredTaskDeliveryAccept @44 () -> ();
  retiredTaskDeliveryReport @45 () -> ();

  # Local governed call controls. The broker retains genuine kernel/launcher
  # process evidence and derives the exact app/workspace/record from this
  # channel. Closed versioned runtime-collab JSON is bounded to 8 KiB.
  nativeMediaCommand @46 (commandJson :Data) -> (replyJson :Data);

  # Disposable committed history read; exact caller and route come from channel.
  committedDocument @47 (request :Data) -> (reply :Data);

  # Invoke one exact lazy row at its required, nonempty directory revision.
  # A separate ordinal makes older brokers reject before any effect. Clients
  # must never fall back to invokePublishedAction when this is unavailable.
  invokeResolvedPublishedAction @48 (
      peerId :UInt64, revision :Text, resource :Text, action :Text,
      argument :Text, hasArgument :Bool, confirmationJson :Text)
      -> (id :UInt64, updateJson :Text);

  # Caller-owned UUID and immutable original target. Unsupported methods must
  # never fall back to @48 or @21. A binding is inert, never caller authority.
  invokeResolvedPublishedActionDurable @49 (
      invocation :Data, binding :PublishedInvocationBinding,
      revision :Text, resource :Text, action :Text,
      argument :Text, hasArgument :Bool, confirmationJson :Text)
      -> (observation :PublishedInvocationObservation);
  publishedInvocationObservation @50 (host :Data, invocation :Data)
      -> (observation :PublishedInvocationObservation);
  cancelDurablePublishedInvocation @51 (host :Data, invocation :Data)
      -> (observation :PublishedInvocationObservation);

  # Settings-only, launcher-authenticated access to the shell's speaker
  # groups: create, edit, identify, calibrate. Devices and outputs travel as
  # the opaque ids the shell publishes; PipeWire identities are not
  # representable.
  speakerCommand @52 (appId :Text, commandJson :Data) -> (replyJson :Data);
  # Resolves once what `speakerCommand`'s snapshot would show (devices and
  # their outputs, speaker groups and their members, calibrations, playback)
  # may have changed since `after`: 0 at first, then the `change` the
  # previous call returned. It carries no device, group or label; the
  # endpoint asks for a fresh snapshot through `speakerCommand`, which
  # re-checks the route and the person's media policy on every answer. An
  # endpoint keeps one call pending instead of asking on a timer. Only the
  # certified Settings route is answered; a host without speaker groups
  # answers unimplemented.
  speakerChanged @68 (after :UInt64) -> (change :UInt64);

  # This application's settings in the session's records: its own
  # `app_personal` and `app_device_local` entries and the shared sections
  # every app reads. The app is the certified route's; there is no way to
  # name another.
  settings @54 () -> (settings :AppSettings);

  # This application's live activity: what it keeps showing outside its
  # windows (a running timer), which the shell presents and whose controls
  # it hands back. The app is the certified route's.
  liveActivity @55 () -> (activity :LiveActivity);

  # Local Files only. Original invocation custody is revalidated when present;
  # offers never delegate it and cannot be redeemed outside this workspace.
  localFileClipboard @56 (commandJson :Data, onBehalfOf :Data) -> (replyJson :Data);

  # Device-local output-only PCM and controls. Caller/app/workspace/record are
  # derived from the launcher-attested channel. No host socket or capture API
  # is returned. The closed command is bounded to 32 KiB and 20 ms stereo PCM.
  audioOutputCommand @57 (command :Data) -> (reply :Data);

  # Execute a host-approved check only for this actual target worker's retained
  # accepted invocation. Request has correlation + check ID, never argv/path/env.
  # Unsupported brokers refuse; clients must not fall back to raw execution.
  approvedCheck @58 (request :Data) -> (receipt :Data);

  # Resolves once this route's channel keys, or this device's access to
  # them, may have changed since `after`: 0 at first, then the `change` the
  # previous call returned. It says only that this route's own channel
  # changed; the endpoint then asks the workspace identity service for the
  # keys, which checks the device and the person's role on every answer. An
  # endpoint holding current keys keeps one call pending instead of asking on
  # a timer. A host without a channel service answers unimplemented, and the
  # endpoint asks on a timer as before.
  channelKeysChanged @59 (after :UInt64) -> (change :UInt64);
  # Informational bounded reports, correlated to that same original invocation.
  # Final receipt remains authoritative. No fallback to raw execution or @58.
  approvedCheckReportedProgress @60 (request :Data) -> (stream :ApprovedCheckReportedProgressStream);

  # Resolves once what `nearbyTransferCommand`'s snapshot would show this
  # connection (devices online, transfers and their phases) may have changed
  # since `after`: 0 at first, then the `change` the previous call returned.
  # It carries no device, transfer or handle; the endpoint asks for a fresh
  # snapshot through `nearbyTransferCommand`, which re-checks the route and
  # the person's continuity policy on every answer. An endpoint keeps one
  # call pending instead of asking on a timer. Only the certified Files
  # route is answered; a host without Nearby Send answers unimplemented.
  nearbyTransferChanged @62 (after :UInt64) -> (change :UInt64);
  # Resolves once the drives the shell lists to Files (through the tools
  # service's drive list: each drive, its size and whether it is open) may
  # have changed since `after`: 0 at first, then the `change` the previous
  # call returned. It carries no drive, label or path; the endpoint asks the
  # tools service for the list again. An endpoint keeps one call pending
  # instead of asking on a timer. Only the certified Files route is
  # answered; a host that cannot say when its drives change answers
  # unimplemented.
  drivesChanged @69 (after :UInt64) -> (change :UInt64);
  # Resolves once the person's apps the tools service lists to Files (each
  # app's id, name and kind) may have changed since `after`: 0 at first,
  # then the `change` the previous call returned. It carries no app; the
  # endpoint asks the tools service for the list again. An endpoint keeps one
  # call pending instead of asking on a timer. Only the certified Files route
  # is answered; a host that cannot say when its apps change answers
  # unimplemented.
  applicationsChanged @70 (after :UInt64) -> (change :UInt64);

  # Private website session custody for the certified Browser Read route.
  # Identity, person and source are derived by the broker, never caller claims.
  browserServiceSessionCommand @63 (command :Data) -> (reply :Data);
  # Person-only private host door. The app, workspace and certificate come
  # from the admitted native worker; onBehalfOf names retained original custody.
  # Admission of an already speculatively applied local Document action.
  # Closed bounded version-1 JSON; caller/app/record derive from this channel.
  # Never redispatches, certifies execution, or authorizes publication.
  admitLocalDocumentAction @66 (request :Data)
      -> (observation :PublishedInvocationObservation);

  personChat @64 (command :Data, onBehalfOf :Data) -> (reply :Data);
  # Native Chat's original person actor only; reauthorized on every next.
  subscribePersonChat @65 (publicKey :Text, onBehalfOf :Data) -> (stream :PersonChatStream);

  # A change to this application's entries in the session's persistent
  # search index: bounded JSON, the vocabulary of
  # `ui::search::index_publication::IndexDelta`. Bound to this certified
  # primary route like reportContinuation (the broker derives workspace and
  # record from it), never relayed to peers. The session keeps the entries
  # after the application closes, so its content stays findable.
  publishSearchIndex @67 (appId :Text, deltaJson :Data) -> ();

}

# Optional result on @35, minted from the same captured directory and worker.
# An absent pointer means reliable invocation is unavailable on this result.
struct PublishedInvocationBinding {
  host @0 :Data;
  peerId @1 :UInt64;
  appId @2 :Text;
  recordId @3 :Text;
  root @4 :Data;
  device @5 :Data;
  targetBinding @6 :Data;
}

struct PublishedInvocationObservation {
  host @0 :Data;
  invocation @1 :Data;
  found @2 :Bool;
  id @3 :UInt64;
  updateJson @4 :Text;
  uncertain @5 :Bool;
  cancellationRequested @6 :Bool;
  # Host-private AdmissionEvidence JSON, returned only to the authorized owner
  # through invocation observation; never included in public streams.
  admissionJson @7 :Text;
  # Host-private terminal evidence JSON, with the same owner-only visibility.
  terminalJson @8 :Text;
}

# Application-owned, broker-pulled resource/action directory. A capability is
# deliberately used instead of a JSON callback: the broker can bound the work
# it requests, preserve typed descriptors, and drop all live rows when the
# connection/capability goes away.
interface ActionDirectory {
  query @0 (query :ActionDirectoryQuery) -> (page :ActionDirectoryPage);
  # An empty `revision` resolves against the directory as it is now; a found
  # resolution then names that revision in `resolvedRevision`.
  resolve @1 (revision :Text, resource :Text, action :Text)
      -> (resolution :ActionDirectoryResolution);
  # Admission of an already-applied local document mutation only. Resolves
  # an exact pre-event revision retained by the SDK until admission completes;
  # never falls back to the current directory or authorizes remote delivery.
  resolveLocalDocumentAction @2 (revision :Text, resource :Text, action :Text)
      -> (resolution :ActionDirectoryResolution);
}

# Caller supplied upper bounds. `maxDepth` counts resource hierarchy edges;
# the source must never walk past it. Zero means "only directly matched rows".
struct ActionDirectoryQuery {
  revision @0 :Text;
  cursor @1 :Data;
  needle @2 :Text;
  parentResource @3 :Text;
  maxResults @4 :UInt32;
  maxBytes @5 :UInt32;
  maxWork @6 :UInt32;
  maxDepth @7 :UInt16;
  # Zero selects the broker's cross-application root directory. A non-zero
  # value pages exactly one live application's capability, identified by the
  # broker-issued peer id returned in the authenticated catalogue.
  peerId @8 :UInt64;
  # Non-empty: only the top-level resources whose kind is this, or whose
  # label is this ignoring ASCII case, answered whole on one page. A name
  # lookup must not depend on where a page budget happens to end.
  resourceName @9 :Text;
}

struct ActionDirectoryRow {
  parentResource @0 :Text;
  resource @1 :Text;
  # A resource row carries an empty action; action rows carry a canonical
  # descriptor. This keeps hierarchy truthful without requiring resource
  # bodies or eagerly serialising every child.
  action @2 :Text;
  descriptor @3 :ExportedAction;
  hasChildren @4 :Bool;
  # Resource-only/container rows use this action+contract solely for
  # visibility. It is evaluated by the broker before the label or hierarchy
  # is forwarded; it does not make the container action invokable.
  visibilityAction @5 :Text;
  visibilityContractJson @6 :Text;
  # Container rows have no descriptor, so their independently bounded display
  # metadata must travel here rather than forcing shells to render raw ids.
  resourceKind @7 :Text;
  resourceLabel @8 :Text;
  # Bounded declarative metadata. The shell parses this into its own themed
  # primitives; application drawing code never crosses this boundary.
  resourcePresentationJson @9 :Text;
  # Empty for legacy publishers. Resource-only rows retain the operation
  # governing disclosure, not just its Read policy contract.
  visibilityGoverningKind @10 :Text;
}

struct ActionDirectoryPage {
  revision @0 :Text;
  rows @1 :List(ActionDirectoryRow);
  nextCursor @2 :Data;
  exhausted @3 :Bool;
  staleCursor @4 :Bool;
  # True when one of the supplied bounds stopped this page. It is distinct
  # from exhaustion so callers can choose larger budgets deliberately.
  budgetExhausted @5 :Bool;
}

struct ActionDirectoryResolution {
  union {
    found @0 :ActionDirectoryRow;
    notFound @1 :Void;
    staleRevision @2 :Void;
    unavailable @3 :Void;
  }
  # For `found`: the revision the owner resolved the row against, which is
  # the named one or, for an empty revision, the directory as it is now. The
  # broker binds the admission to it, so a host worker refuses to run the
  # action once its directory moved on. Empty from an owner built before
  # this field existed.
  resolvedRevision @4 :Text;
}

interface WorkspaceControlHandle {
  # Postcard-encoded runtime_collab::WorkspaceControlEvent.
  read @0 () -> (event :Data);
  # Postcard-encoded runtime_collab::WorkspaceControlInput. Refused until the
  # target's stream-start consent edge and after any close/revocation edge.
  sendInput @1 (event :Data) -> ();
  close @2 () -> ();
}

interface ActionCatalogueStream {
  # Level-triggered: the first read returns the current complete directory,
  # then each publication/disconnect returns a fresh complete replacement.
  read @0 () -> (catalogueJson :Text);
}

interface RealtimeStream {
  # Versioned tilo-app RealtimeEvent JSON. Client-pull preserves bounded
  # backpressure and prevents an idle app from accumulating callbacks.
  read @0 () -> (eventJson :Data);
}

# One operation an application exports, as inert data.
#
# Mirrors `tilo_actions::door::ActionDescriptor`. A native capnp struct
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
  # `document`, `effect`, or `view`.
  operationKind @7 :Text;
  # JSON encoding of `tilo_actions::ActionParam[]`. Kept as an opaque JSON
  # projection so Cap'n Proto clients can forward newer parameter vocabulary
  # without a schema bump.
  paramsJson @8 :Text;
  # Versioned JSON encoding of tilo_actions::ActionContract.
  contractJson @9 :Text;
  # Optional explicit resource-view metadata. Absence means this action is
  # invokable but is not advertised as a view for its resource.
  view @10 :ResourceViewDescriptor;
  # Descriptive source identity, never permission or an invocation alias.
  semanticIdentity @11 :ResourceSemanticIdentity;
}

struct ResourceSemanticIdentity {
  kind @0 :Text;
  key @1 :Text;
}

struct ResourceViewDescriptor {
  id          @0 :Text;
  label       @1 :Text;
  # Empty when the app supplied no description.
  description @2 :Text;
  openAction  @3 :Text;
  primary     @4 :Bool;
  mount       @5 :ResourceViewMountSpec;
  range       @6 :ResourceViewRangeSpec;
}

struct ResourceViewMountSpec {
  action          @0 :Text;
  capabilityParam @1 :Text;
  viewParam       @2 :Text;
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
  # invocationUuid is the complete 16-byte logical identity. id remains the
  # local completion/cancellation handle.
  read @0 () -> (id :UInt64, resource :Text, action :Text,
                 argument :Text, hasArgument :Bool, actor :Text,
                 contextJson :Text, invocationUuid :Data);
}

# Agent-only task work sent by the broker after live-session admission.
interface TaskRunStream {
  read @0 () -> (taskJson :Text);
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
  #
  # `admission` is the broker's verdict for this op on its channel: the
  # broker alone decides whether cleartext is admitted, and the app applies
  # that verdict.
  read @0 () -> (op :Data, fromUserId :Text, kind :OpKind, admission :OpAdmission);
}

enum OpKind {
  # Every live stream frame is a postcard-encoded SignedOp.
  signed @0;
}

enum OpAdmission {
  # A broker that predates the verdict.
  unstated @0;
  # The channel admits cleartext for this op.
  cleartextAdmitted @1;
  # The channel requires this op sealed.
  encryptionRequired @2;
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

  # Publish truthful, semantic action activity for a stable resource. The
  # authenticated channel member is always the actor; `interactionJson` must
  # not contain (and cannot override) identity. Unlike pointer/caret presence,
  # this describes what an action is doing without inventing input gestures.
  publishInteraction @9 (keyBytes :Data, interactionJson :Text) -> ();

  # Short transient intention. Empty text clears it; automatic distinguishes
  # a local-model suggestion from words the participant explicitly authored.
  publishStatus @10 (keyBytes :Data, text :Text, automatic :Bool) -> ();
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
  # Pull the next news of content for this channel. Promise resolves when
  # the broker has some.
  #
  # `hash` is the blake3 hex the content is addressed by in every store
  # on every machine. With `state` `arrived`, `bytes` are the content. A
  # reader MUST verify the bytes against it before storing them: the key
  # *is* the content, so bytes that hash to something else are not what
  # was asked for, and taking them on trust would let one peer poison
  # every replica that later reads that hash.
  #
  # Any other `state` says where a `wantBlob` of `hash` has got to, and
  # `bytes` are empty. It changes as devices that could hand the content
  # over connect and leave, never on a timer.
  read @0 () -> (hash :Text, bytes :Data, state :BlobState);
}

enum BlobState {
  # The content is here: `bytes` carry it.
  arrived @0;
  # A connected device that could hand it over is being asked for it.
  fetching @1;
  # No device that could hand it over is connected right now.
  waiting @2;
  # Every connected device that could hold it said it does not.
  unavailable @3;
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

    # Ephemeral visual relationship presence. The authenticated event sender
    # is following this participant; an empty value clears the cue. This is
    # never a request to alter the receiver's viewport.
    following @11 :Text;

    # Versioned tilo_actions::InteractionUpdate JSON. Kept as an additive
    # union arm so non-Rust peers can preserve/forward evolving semantic
    # targets without understanding app-specific anchor variants.
    interactionJson @12 :Text;

    status :group {
      text @13 :Text;
      automatic @14 :Bool;
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
  # A session-local Follow command. `followChanged == false` means this is
  # the ordinary complete roster/permission snapshot above. When true,
  # clients leave that snapshot untouched and either follow `followTarget`
  # or stop when it is empty. The compositor never relays this to peers.
  followTarget  @2 :Text;
  followChanged @3 :Bool;
  # Directed control frame for one selected document holder. Ordinary context
  # snapshots and follow controls leave this false.
  snapshotRequested @4 :Bool;
  # Session-local handoff control for exactly one application instance,
  # addressed by its primary route: the shell asking it to make its document
  # durable, or to record that its window moved to another workspace. The
  # JSON vocabulary is `ui::collaboration::handoff`. Empty on every other
  # frame; never relayed to peers.
  handoff @5 :Data;
  # Session-local control frame: the delegated agents on this room's roster
  # whose person's root signed the delegation for exactly that device, as
  # the session verified them. `historyAgentsChanged == true` means clients
  # leave the roster snapshot untouched and replace the room's agents with
  # `historyAgents` (empty: none). History names who wrote from it; it is
  # presentation only and grants nothing. Additive: a client that predates
  # it reads an empty roster frame, and roster rows are upserts.
  historyAgents @6 :List(HistoryAgent);
  historyAgentsChanged @7 :Bool;
  # Session-local control frame: records of this application's trees (a
  # chain's next part, a container's child) that a peer wrote and no window
  # here holds. Clients leave the roster snapshot untouched and hold them as
  # their trees' own. Empty on every other frame; never relayed to peers.
  # Additive: a client that predates it reads an empty roster frame.
  recordsArrived @8 :List(Text);
}

# One verified delegated agent in a `ChannelContext.historyAgents` frame.
struct HistoryAgent {
  # The agent's own 32-byte device key: the actor its signed changes carry.
  device     @0 :Data;
  # The 32-byte root key of the person it acts for.
  person     @1 :Data;
  # That person's name as the room's governance records it.
  personName @2 :Text;
  # The delegation's stable agent id.
  agentId    @3 :Text;
  # The agent's display name.
  label      @4 :Text;
  # Its cursor colour, packed 0x00RRGGBB like `Collaborator.color`.
  color      @5 :UInt32;
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
  # When the room admitted this member, Unix milliseconds, as its
  # membership records it. People who share a name are numbered in this
  # order, so a newcomer never takes an earlier member's number. The
  # default, all ones, is a member the room has no time for (an agent, or
  # one admitted before rooms recorded it), which is also what a peer that
  # predates this field decodes as.
  joinedAtMs  @5 :UInt64 = 0xffffffffffffffff;
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

# One application's view of the session's settings, from `settings`.
# Documents are JSON objects of sections, at most 1 MiB.
interface LiveActivity {
  # Show `activityJson`, a JSON object, as the app's live activity in place
  # of the one shown; empty bytes end it.
  show @0 (activityJson :Data) -> ();
  # The controls pressed on the app's live activity after `after`, once
  # there is one: a JSON array of the controls, and the serial of the last.
  controls @1 (after :UInt64) -> (serial :UInt64, controlsJson :Data);
}

enum DisplayRequestOrigin {
  unavailable @0;
  localHost @1;
  published @2;
}

interface AppSettings {
  # The view once its revision differs from `after`; at once for 0. A
  # caller follows changes by passing back the revision it was given.
  read @0 (after :UInt64) -> (revision :UInt64, documentJson :Data);
  # Merge `valuesJson`, `{"<app id>": {...}}`, into the app's entry in
  # `section` (`app_personal` or `app_device_local`), answering the view
  # after the write. Any other section or entry is refused. `action` names
  # the action that made the change, kept with it in the session's history;
  # empty when none did.
  update @1 (section :Text, valuesJson :Data, action :Text) -> (revision :UInt64, documentJson :Data);
  # Narrow display intent over an independently admitted private Settings
  # worker. The host owns validation, prior state and transaction persistence.
  displayControl @2 (commandJson :Data, publishedInvocation :Data, origin :DisplayRequestOrigin) -> (replyJson :Data);
}

struct ResourceViewRangeSpec {
  action @0 :Text;
  startParam @1 :Text;
  endParam @2 :Text;
  unitParam @3 :Text;
}

# One catalogue entry of a published surface: an action's id, labels,
# description, input, parameters, contract and resource view, with nothing of
# the resource it is offered on. `id` is a content hash the publisher chose;
# the broker keys the entry by it and trusts nothing else about it.
struct ActionShape {
  id @0 :Data;
  # An `ExportedAction` whose resource, resourceKind, resourceLabel and
  # semanticIdentity are empty.
  action @1 :ExportedAction;
}

# An ordered set of shapes a resource offers, named by a content hash.
struct ActionTemplate {
  id @0 :Data;
  # The shape ids, concatenated, each of the same fixed length.
  shapes @1 :Data;
}

# One resource of a published surface: the fields that are its own, and the
# template of the actions it offers.
struct ActionResource {
  resource @0 :Text;
  resourceKind @1 :Text;
  resourceLabel @2 :Text;
  semanticIdentity @3 :ResourceSemanticIdentity;
  template @4 :Data;
}

# Private participant capability for one selected pair, never a broadcast.
# next carries counts only; read freshly authorizes and returns a bounded snapshot.
interface PersonChatStream {
  next @0 () -> (generation :UInt64);
  read @1 () -> (reply :Data);
}

# Each read returns exactly one nonempty field. Receipt/error freezes the lane.
interface ApprovedCheckReportedProgressStream {
  read @0 () -> (report :Data, receipt :Data, error :Text);
}
