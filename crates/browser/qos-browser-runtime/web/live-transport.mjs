/**
 * LIVE_V1 MoQ Draft 21 transport over WebTransport at `/moq/v21/live`,
 * mirroring the voice path (web/media-moq.mjs): the host-signed live grant
 * is the Authorization Token, the vendored library is used through its
 * public API only.
 *
 * Tracks (see tilo-relay delivery_v3/live_wire.rs):
 *   namespace ("tilo-live", hex(room digest), hex(publisher device))
 *   `presence`             latest-value OBJECT_DATAGRAMs (single-frame groups when
 *                          datagrams are unavailable)
 *   `ops/<hex record>`     ordered frames on streams, one group per burst
 */
const DRAFT_VERSION = 'moq-transport-21';
const DRAFT_PROTOCOL = 'moqt-21';
export const LIVE_PATH = '/moq/v21/live';
export const LIVE_ROOT = 'tilo-live';
export const PRESENCE_TRACK = 'presence';
export const MAX_DATAGRAM_FRAME_BYTES = 1100;
export const MAX_STREAM_FRAME_BYTES = 64 * 1024;
export const MAX_OPEN_RECORDS = 16;
/** Relay egress limit per namespace; bounds tracks served on request. */
export const MAX_SERVED_RECORDS = 64;
const MAX_AUTHORIZATION_BYTES = 2048;
const MAX_REMOTE_PUBLISHERS = 64;
const OPS_GROUP_IDLE_MS = 500;
const OPS_GROUP_FRAMES = 256;
// Ordered ops subscriptions: latency_max >= 2 s so the relay never skips an
// older group when a newer one arrives first (each forwarded SignedChange is
// its own group), and enough buffered groups that a burst is never evicted
// before it is read.
const OPS_SUBSCRIPTION = Object.freeze({priority: 1, ordered: true, latencyMax: 2500, maxGroups: 256, maxFrames: 1024, maxAgeMs: 5000});
const HEX64 = /^[0-9a-f]{64}$/;
const OPS_TRACK = /^ops\/[0-9a-f]{64}$/;

export function validateLiveEndpoint(value) {
  const endpoint = new URL(value instanceof URL ? value.href : value);
  if (endpoint.protocol !== 'https:' || endpoint.username || endpoint.password
    || endpoint.search || endpoint.hash || endpoint.pathname !== LIVE_PATH) {
    throw new TypeError('live endpoint must be an exact credential-free HTTPS /moq/v21/live URL');
  }
  return endpoint;
}
function validateNamespace(value) {
  if (!Array.isArray(value) || value.length !== 3 || value[0] !== LIVE_ROOT || !HEX64.test(value[1]) || !HEX64.test(value[2])) {
    throw new TypeError('live namespace must be ("tilo-live", room hex, publisher hex)');
  }
  return [...value];
}

export function createLiveTransport({library, serverCertificateHashes, clock = {}, onReport = () => {}} = {}) {
  const monotonicNow = clock.now ?? (() => performance.now());
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (handle => globalThis.clearTimeout(handle));
  const report = value => { try { onReport(value); } catch {} };
  const libraryPromise = library ? Promise.resolve(library) : import('./media-moq-library.mjs');

  async function connect({endpoint, authorization, namespace, directions, signal}) {
    signal?.throwIfAborted?.();
    const url = validateLiveEndpoint(endpoint);
    const [root, room, self] = validateNamespace(namespace);
    if (!(authorization instanceof Uint8Array) || authorization.byteLength === 0 || authorization.byteLength > MAX_AUTHORIZATION_BYTES) {
      throw new TypeError('live authorization must be 1..2048 grant bytes');
    }
    if (!directions || (!directions.publish && !directions.subscribe)) throw new TypeError('live directions required');
    const moq = await libraryPromise;
    const webtransport = {protocols: [DRAFT_PROTOCOL]};
    if (serverCertificateHashes) webtransport.serverCertificateHashes = serverCertificateHashes;
    const connection = await moq.Connection.connect(url, {
      authorization: new Uint8Array(authorization),
      websocket: {enabled: false},
      webtransport,
      signal,
    });
    if (signal?.aborted) { connection.close(); throw signal.reason; }
    if (connection.version !== DRAFT_VERSION || connection.transport !== 'webtransport') {
      connection.close();
      throw new Error('live connection did not negotiate moq-transport-21 over WebTransport');
    }

    let closed = false;
    let broadcast;
    let presenceTrack;
    let datagrams = false;
    const opsTracks = new Map(); // name -> {track, group, frames, timer, usedAt}
    const cleanups = new Set();
    const safeClose = resource => { try { resource?.close?.(); } catch {} };
    const stats = {presenceSent: 0, opsSent: 0, presenceReceived: 0, opsReceived: 0, mode: 'streams'};

    const close = () => {
      if (closed) return;
      closed = true;
      signal?.removeEventListener?.('abort', close);
      for (const cleanup of [...cleanups]) { try { cleanup(); } catch {} }
      cleanups.clear();
      for (const entry of opsTracks.values()) { if (entry.timer !== undefined) clearTimer(entry.timer); safeClose(entry.group); safeClose(entry.track); }
      opsTracks.clear();
      for (const track of served.values()) safeClose(track);
      served.clear();
      safeClose(presenceTrack);
      safeClose(broadcast);
      safeClose(connection);
    };
    signal?.addEventListener?.('abort', close, {once: true});
    const closedPromise = Promise.resolve(connection.closed).then(() => close(), () => close());

    try {
      if (directions.publish) {
        broadcast = new moq.Broadcast.Producer();
        presenceTrack = broadcast.createTrack(PRESENCE_TRACK, {
          timescale: moq.Time.Timescale.MICRO,
          latencyMax: 50,
          ordered: false,
        }, {maxGroups: 1, maxFrames: 1, maxAgeMs: 100});
        connection.publish(moq.Path.from(root, room, self), broadcast);
        datagrams = typeof presenceTrack.appendDatagram === 'function';
        stats.mode = datagrams ? 'datagrams' : 'streams';
      }
    } catch (error) {
      close();
      throw error;
    }
    const timestamp = () => new moq.Time.Timestamp(Math.max(0, Math.round(monotonicNow() * 1000)), moq.Time.Timescale.MICRO);

    function publishPresence(bytes) {
      if (closed || !presenceTrack) return false;
      if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > MAX_DATAGRAM_FRAME_BYTES) throw new RangeError('presence frame exceeds datagram bound');
      if (datagrams) presenceTrack.appendDatagram(timestamp(), bytes);
      else presenceTrack.writeFrame({payload: bytes, timestamp: timestamp()});
      stats.presenceSent++;
      return true;
    }
    function finishGroup(entry) {
      if (entry.timer !== undefined) clearTimer(entry.timer);
      entry.timer = undefined;
      safeClose(entry.group);
      entry.group = undefined;
      entry.frames = 0;
    }
    // Records this client is actively writing (<= 16, LRU) versus tracks the
    // relay subscribed on demand because members have the record open
    // (<= 64, idle until this client writes). Only written records count
    // against the relay's open-record limit.
    const served = new Map(); // name -> track
    function closeOps(name) {
      const entry = opsTracks.get(name);
      if (!entry) return;
      opsTracks.delete(name);
      finishGroup(entry);
      safeClose(entry.track);
      try { broadcast?.removeTrack?.(name); } catch {}
    }
    const OPS_INFO = () => ({timescale: moq.Time.Timescale.MICRO, latencyMax: 2000, ordered: true});
    const usable = track => track && track.closed?.peek?.() === undefined;
    function opsEntry(name) {
      let entry = opsTracks.get(name);
      if (entry && usable(entry.track)) return entry;
      if (entry) closeOps(name);
      if (opsTracks.size >= MAX_OPEN_RECORDS) {
        const oldest = [...opsTracks.entries()].sort((a, b) => a[1].usedAt - b[1].usedAt)[0][0];
        closeOps(oldest);
      }
      let track = served.get(name);
      served.delete(name);
      if (!usable(track)) {
        try { broadcast.removeTrack(name); } catch {}
        track = broadcast.createTrack(name, OPS_INFO(), {maxGroups: 4, maxFrames: OPS_GROUP_FRAMES * 2, maxAgeMs: 2000});
      }
      entry = {track, group: undefined, frames: 0, timer: undefined, usedAt: monotonicNow()};
      opsTracks.set(name, entry);
      return entry;
    }
    function serve(request) {
      // Inserted tracks are subscribed directly; a request means the track is
      // not (or no longer) inserted, so serve it and keep it for writing.
      for (const [name, track] of served) if (!usable(track)) served.delete(name);
      if (served.size >= MAX_SERVED_RECORDS) { request.reject(new Error('live ops egress limit')); return; }
      const track = request.accept(OPS_INFO());
      try { broadcast.insertTrack(track); } catch {}
      const entry = opsTracks.get(request.name);
      if (entry) {
        // Replaces a closed track of an actively written record.
        finishGroup(entry);
        entry.track = track;
      } else {
        served.set(request.name, track);
      }
    }
    function serveRequests() {
      if (!broadcast) return;
      void (async () => {
        try {
          for (;;) {
            const request = await broadcast.requested();
            if (!request || closed) return;
            if (OPS_TRACK.test(request.name)) {
              try { serve(request); } catch (error) { report({type: 'live-ops-request-error', error}); }
            } else {
              request.reject(new Error('unknown live track'));
            }
          }
        } catch (error) {
          if (!closed) report({type: 'live-request-error', error});
        }
      })();
    }
    serveRequests();
    function publishOps(name, bytes) {
      if (closed || !broadcast) return false;
      if (!OPS_TRACK.test(name)) throw new TypeError('invalid ops track name');
      if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > MAX_STREAM_FRAME_BYTES) throw new RangeError('ops frame exceeds stream bound');
      const entry = opsEntry(name);
      entry.usedAt = monotonicNow();
      if (!entry.group) entry.group = entry.track.appendGroup();
      entry.group.writeFrame({payload: bytes, timestamp: timestamp()});
      entry.frames++;
      stats.opsSent++;
      if (entry.timer !== undefined) clearTimer(entry.timer);
      entry.timer = undefined;
      if (entry.frames >= OPS_GROUP_FRAMES) finishGroup(entry);
      else entry.timer = setTimer(() => { entry.timer = undefined; finishGroup(entry); }, OPS_GROUP_IDLE_MS);
      return true;
    }

    /** Announcements of other members under ("tilo-live", room). */
    function watchMembers({onMember}) {
      if (closed) throw new Error('live session closed');
      if (!directions.subscribe) throw new Error('live session not authorized to subscribe');
      let active = true;
      const announcements = connection.announced(moq.Path.from(root, room));
      const known = new Set();
      const cleanup = () => {
        if (!active) return;
        active = false;
        cleanups.delete(cleanup);
        safeClose(announcements);
      };
      cleanups.add(cleanup);
      void (async () => {
        try {
          while (active) {
            const event = await announcements.next();
            if (!event) break;
            if (!HEX64.test(event.path) || event.path === self) continue;
            if (event.active === true && known.has(event.path)) {
              // A fresh announcement of a known member (withdrawn and re-announced,
              // e.g. host-fed namespace): drop and re-subscribe.
              try { onMember(event.path, false); } catch {}
              try { onMember(event.path, true); } catch {}
            } else if (event.active === true) {
              if (known.size >= MAX_REMOTE_PUBLISHERS) { report({type: 'live-publisher-limit'}); continue; }
              known.add(event.path);
              try { onMember(event.path, true); } catch {}
            } else if (event.active === false && known.delete(event.path)) {
              try { onMember(event.path, false); } catch {}
            }
          }
        } catch (error) {
          if (active) report({type: 'live-announcement-error', error});
        } finally {
          if (active) cleanup();
        }
      })();
      return cleanup;
    }

    function consumer(publisher) {
      if (!HEX64.test(publisher) || publisher === self) throw new TypeError('invalid remote publisher');
      return connection.consume(moq.Path.from(root, room, publisher));
    }
    // The member's broadcast closes when its namespace is withdrawn
    // (revocation, host-fed re-announce). Datagram-only subscriptions do not
    // end by themselves, so end them here; the caller re-subscribes on the
    // fresh announcement.
    function whenBroadcastCloses(remote, end) {
      const closedSignal = remote?.closed;
      if (closedSignal && typeof closedSignal.then === 'function') closedSignal.then(end, end);
    }
    /** Presence datagrams (and single-frame groups) from one member. */
    function subscribePresence(publisher, onFrame, onEnd = () => {}) {
      if (closed) throw new Error('live session closed');
      let active = true;
      const remote = consumer(publisher);
      const track = remote.track(PRESENCE_TRACK).subscribe({priority: 0, ordered: false, latencyMax: 50, maxGroups: 1, maxFrames: 1, maxAgeMs: 100});
      const deliver = payload => {
        if (!active) return;
        stats.presenceReceived++;
        try { onFrame(payload, monotonicNow()); } catch (error) { report({type: 'live-frame-error', error}); }
      };
      const cleanup = () => {
        if (!active) return;
        active = false;
        cleanups.delete(cleanup);
        safeClose(track);
        safeClose(remote);
      };
      const ended = () => { if (active) { cleanup(); try { onEnd(); } catch {} } };
      cleanups.add(cleanup);
      if (typeof track.recvDatagram === 'function') {
        void (async () => {
          try { for (;;) { const datagram = await track.recvDatagram(); if (!datagram || !active) return; deliver(datagram.payload); } }
          catch (error) { if (active) report({type: 'live-subscription-error', publisher, error}); }
        })();
      }
      void (async () => {
        try {
          for (;;) {
            let group = await track.recvGroup();
            if (!active) return;
            if (!group) break;
            // Latest value: skip to the newest buffered group.
            for (;;) { const newer = track.tryRecvGroup?.(); if (!newer) break; safeClose(group); group = newer; }
            try { const frame = await group.readFrame(); if (frame && active) deliver(frame.payload); }
            catch (error) { report({type: 'live-presence-group-error', publisher, error}); }
            finally { safeClose(group); }
          }
        } catch (error) {
          if (active) report({type: 'live-subscription-error', publisher, error});
        }
        // Publisher revoked or egress withdrawn (stream reset / track end).
        ended();
      })();
      whenBroadcastCloses(remote, ended);
      return cleanup;
    }
    /** Ordered ops frames for one record from one member. */
    function subscribeOps(publisher, name, onFrame, onEnd = () => {}) {
      if (closed) throw new Error('live session closed');
      if (!OPS_TRACK.test(name)) throw new TypeError('invalid ops track name');
      let active = true;
      const remote = consumer(publisher);
      const track = remote.track(name).subscribe({...OPS_SUBSCRIPTION});
      const cleanup = () => {
        if (!active) return;
        active = false;
        cleanups.delete(cleanup);
        safeClose(track);
        safeClose(remote);
      };
      const ended = () => { if (active) { cleanup(); try { onEnd(); } catch {} } };
      cleanups.add(cleanup);
      void (async () => {
        try {
          for (;;) {
            const group = await track.recvGroup();
            if (!active) return;
            if (!group) break;
            try {
              for (;;) {
                const frame = await group.readFrame();
                if (!frame || !active) break;
                stats.opsReceived++;
                try { onFrame(frame.payload, monotonicNow()); } catch (error) { report({type: 'live-frame-error', error}); }
              }
            } catch (error) {
              report({type: 'live-ops-group-error', publisher, error});
            } finally { safeClose(group); }
          }
        } catch (error) {
          if (active) report({type: 'live-subscription-error', publisher, error});
        }
        ended();
      })();
      whenBroadcastCloses(remote, ended);
      return cleanup;
    }

    return {
      namespace: [root, room, self],
      directions: {...directions},
      publishPresence,
      publishOps,
      closeOps,
      watchMembers,
      subscribePresence,
      subscribeOps,
      stats: () => ({...stats, openRecords: opsTracks.size, servedRecords: served.size}),
      close,
      closed: closedPromise,
    };
  }
  return {connect};
}
