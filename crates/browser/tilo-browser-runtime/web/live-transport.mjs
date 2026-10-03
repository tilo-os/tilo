/**
 * LIVE_V1 `moq-lite` transport over WebTransport at `/moq/live`, mirroring
 * the voice path (web/media-moq.mjs): the host-signed live grant is presented
 * in the carrier's admission preface (web/moq-carrier.mjs), and `@moq/net` is
 * used through its public API only.
 *
 * Broadcasts (see tilo-relay delivery_v3/live_wire.rs):
 *   `tilo-live/<room hex>/<publisher hex>`   this member's namespace, with
 *     `presence`             latest-value datagrams (single-frame groups when
 *                            datagrams are unavailable)
 *   `<namespace>/ops/<hex record>`           one per record this member is
 *     writing, with one ordered track `ops`; announcing it opens the record
 * The relay re-publishes each member as one namespace broadcast carrying
 * `presence` and on-demand `ops/<hex record>` tracks.
 */
import {MOQ_PROTOCOL, openCarrier, requestBroadcast} from './moq-carrier.mjs';

export const LIVE_PATH = '/moq/live';
export const LIVE_ROOT = 'tilo-live';
export const PRESENCE_TRACK = 'presence';
export const RECORD_TRACK = 'ops';
export const MAX_DATAGRAM_FRAME_BYTES = 1100;
export const MAX_STREAM_FRAME_BYTES = 64 * 1024;
export const MAX_OPEN_RECORDS = 16;
const MAX_AUTHORIZATION_BYTES = 2048;
const MAX_REMOTE_PUBLISHERS = 64;
const OPS_GROUP_IDLE_MS = 500;
const OPS_GROUP_FRAMES = 256;
// Ordered ops subscriptions tolerate 2.5 s of reordering so the relay never
// skips an older group when a newer one arrives first (each forwarded
// SignedChange is its own group).
const OPS_SUBSCRIPTION = Object.freeze({priority: 1, maxAge: 2500});
const PRESENCE_SUBSCRIPTION = Object.freeze({priority: 0, maxAge: 50});
const HEX64 = /^[0-9a-f]{64}$/;
const OPS_TRACK = /^ops\/[0-9a-f]{64}$/;

const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

/** An HTTPS `/moq/live` URL, or a loopback `http:` one (a local relay's
 * `--web-plain` origin, which only the HTTPS stream fallback can use). */
export function validateLiveEndpoint(value) {
  const endpoint = new URL(value instanceof URL ? value.href : value);
  const secure = endpoint.protocol === 'https:' || (endpoint.protocol === 'http:' && LOOPBACK.has(endpoint.hostname));
  if (!secure || endpoint.username || endpoint.password
    || endpoint.search || endpoint.hash || endpoint.pathname !== LIVE_PATH) {
    throw new TypeError('live endpoint must be an exact credential-free HTTPS /moq/live URL');
  }
  return endpoint;
}
function validateNamespace(value) {
  if (!Array.isArray(value) || value.length !== 3 || value[0] !== LIVE_ROOT || !HEX64.test(value[1]) || !HEX64.test(value[2])) {
    throw new TypeError('live namespace must be ("tilo-live", room hex, publisher hex)');
  }
  return [...value];
}

/**
 * `library` is `@moq/net` (the bundled media-moq-library.mjs by default);
 * `carrier` opens an admitted WebTransport session (web/moq-carrier.mjs).
 * Tests inject both.
 */
export function createLiveTransport({library, carrier = openCarrier, serverCertificateHashes, clock = {}, onReport = () => {}} = {}) {
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
    const own = moq.Path.from(root, room, self);
    const roomPath = moq.Path.from(root, room);
    const outgoing = new moq.Origin.Producer();
    const incoming = new moq.Origin.Producer();
    const opened = await carrier({url, grant: new Uint8Array(authorization), serverCertificateHashes, signal});
    let connection;
    try {
      connection = await moq.Connection.connect({
        url,
        transport: opened.transport,
        publish: directions.publish ? outgoing.consume() : undefined,
        consume: incoming,
        signal,
      });
    } catch (error) {
      opened.close();
      throw error;
    }
    if (signal?.aborted) { connection.close(); opened.close(); throw signal.reason; }
    if (connection.version !== MOQ_PROTOCOL || connection.transport !== 'webtransport') {
      connection.close();
      opened.close();
      throw new Error(`live connection did not negotiate ${MOQ_PROTOCOL} over WebTransport`);
    }

    let closed = false;
    let broadcast;
    let presenceTrack;
    const records = new Map(); // track name -> {broadcast, track, group, frames, timer, usedAt}
    const cleanups = new Set();
    const safeClose = resource => { try { resource?.close?.(); } catch {} };
    const stats = {presenceSent: 0, opsSent: 0, presenceReceived: 0, opsReceived: 0, mode: 'datagrams'};

    const close = () => {
      if (closed) return;
      closed = true;
      signal?.removeEventListener?.('abort', close);
      for (const cleanup of [...cleanups]) { try { cleanup(); } catch {} }
      cleanups.clear();
      for (const entry of records.values()) { if (entry.timer !== undefined) clearTimer(entry.timer); safeClose(entry.group); safeClose(entry.track); safeClose(entry.broadcast); }
      records.clear();
      safeClose(presenceTrack);
      safeClose(broadcast);
      safeClose(connection);
      safeClose(outgoing);
      safeClose(incoming);
      opened.close();
    };
    signal?.addEventListener?.('abort', close, {once: true});
    const closedPromise = Promise.race([Promise.resolve(connection.closed), opened.revoked]).then(() => close(), () => close());

    try {
      if (directions.publish) {
        broadcast = outgoing.createBroadcast(own);
        presenceTrack = broadcast.createTrack(PRESENCE_TRACK, {timescale: moq.Time.Timescale.MICRO, maxAge: 100});
        broadcast.announce();
      }
    } catch (error) {
      close();
      throw error;
    }
    const timestamp = () => new moq.Time.Timestamp(Math.max(0, Math.round(monotonicNow() * 1000)), moq.Time.Timescale.MICRO);

    function publishPresence(bytes) {
      if (closed || !presenceTrack) return false;
      if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > MAX_DATAGRAM_FRAME_BYTES) throw new RangeError('presence frame exceeds datagram bound');
      presenceTrack.appendDatagram(timestamp(), bytes);
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
    // Records this client is actively writing (<= 16, LRU). Each is its own
    // announced broadcast; the relay opens it on announcement and closes it
    // when the broadcast ends.
    function closeOps(name) {
      const entry = records.get(name);
      if (!entry) return;
      records.delete(name);
      finishGroup(entry);
      safeClose(entry.track);
      safeClose(entry.broadcast);
    }
    const usable = track => track && track.closed?.peek?.() === undefined;
    function recordEntry(name) {
      let entry = records.get(name);
      if (entry && usable(entry.track)) return entry;
      if (entry) closeOps(name);
      if (records.size >= MAX_OPEN_RECORDS) {
        const oldest = [...records.entries()].sort((a, b) => a[1].usedAt - b[1].usedAt)[0][0];
        closeOps(oldest);
      }
      const recordBroadcast = outgoing.createBroadcast(moq.Path.from(root, room, self, name));
      const track = recordBroadcast.createTrack(RECORD_TRACK, {timescale: moq.Time.Timescale.MICRO, maxAge: 2000});
      recordBroadcast.announce();
      entry = {broadcast: recordBroadcast, track, group: undefined, frames: 0, timer: undefined, usedAt: monotonicNow()};
      records.set(name, entry);
      return entry;
    }
    function publishOps(name, bytes) {
      if (closed || !broadcast) return false;
      if (!OPS_TRACK.test(name)) throw new TypeError('invalid ops track name');
      if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > MAX_STREAM_FRAME_BYTES) throw new RangeError('ops frame exceeds stream bound');
      const entry = recordEntry(name);
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
      const announcements = incoming.announced(moq.Path.Pattern.subtree(roomPath));
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
            const member = moq.Path.stripPrefix(roomPath, event.prefix);
            if (!member || !HEX64.test(member) || member === self) continue;
            const live = moq.Announce?.isActive ? moq.Announce.isActive(event.kind) : event.kind !== 'retracted';
            if (live && event.kind === 'announced' && known.has(member)) {
              // A fresh announcement of a known member (withdrawn and re-announced,
              // e.g. host-fed namespace): drop and re-subscribe.
              try { onMember(member, false); } catch {}
              try { onMember(member, true); } catch {}
            } else if (live && !known.has(member)) {
              if (known.size >= MAX_REMOTE_PUBLISHERS) { report({type: 'live-publisher-limit'}); continue; }
              known.add(member);
              try { onMember(member, true); } catch {}
            } else if (!live && known.delete(member)) {
              try { onMember(member, false); } catch {}
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

    /**
     * Subscribe to `name` on `publisher`'s egress broadcast. Resolves the
     * broadcast first; `onEnd` runs once when the subscription or the
     * member's broadcast ends (revocation, host-fed re-announce).
     */
    function subscribeTrack(publisher, name, options, run, onEnd) {
      if (closed) throw new Error('live session closed');
      if (!HEX64.test(publisher) || publisher === self) throw new TypeError('invalid remote publisher');
      let active = true;
      let request;
      let track;
      const cleanup = () => {
        if (!active) return;
        active = false;
        cleanups.delete(cleanup);
        safeClose(track);
        safeClose(request);
      };
      const ended = () => { if (active) { cleanup(); try { onEnd(); } catch {} } };
      cleanups.add(cleanup);
      void (async () => {
        try {
          const resolved = await requestBroadcast(incoming, moq.Path.from(root, room, publisher));
          request = resolved.request;
          if (!active) { request.close(); return; }
          track = resolved.broadcast.track(name).subscribe({...options});
          const closedSignal = resolved.broadcast.closed;
          if (closedSignal && typeof closedSignal.then === 'function') closedSignal.then(ended, ended);
          await run(track, () => active);
        } catch (error) {
          if (active) report({type: 'live-subscription-error', publisher, error});
        }
        ended();
      })();
      return cleanup;
    }

    /** Presence datagrams (and single-frame groups) from one member. */
    function subscribePresence(publisher, onFrame, onEnd = () => {}) {
      const deliver = (payload, active) => {
        if (!active()) return;
        stats.presenceReceived++;
        try { onFrame(payload, monotonicNow()); } catch (error) { report({type: 'live-frame-error', error}); }
      };
      return subscribeTrack(publisher, PRESENCE_TRACK, PRESENCE_SUBSCRIPTION, async (track, active) => {
        const datagrams = (async () => {
          try { for (;;) { const datagram = await track.recvDatagram(); if (!datagram || !active()) return; deliver(datagram.payload, active); } }
          catch (error) { if (active()) report({type: 'live-subscription-error', publisher, error}); }
        })();
        for (;;) {
          let group = await track.recvGroup();
          if (!active() || !group) break;
          // Latest value: skip to the newest buffered group.
          for (;;) { const newer = track.tryRecvGroup(); if (!newer) break; safeClose(group); group = newer; }
          try { const frame = await group.readFrame(); if (frame?.payload?.byteLength) deliver(frame.payload, active); }
          catch (error) { report({type: 'live-presence-group-error', publisher, error}); }
          finally { safeClose(group); }
        }
        await datagrams;
      }, onEnd);
    }
    /** Ordered ops frames for one record from one member. */
    function subscribeOps(publisher, name, onFrame, onEnd = () => {}) {
      if (!OPS_TRACK.test(name)) throw new TypeError('invalid ops track name');
      return subscribeTrack(publisher, name, OPS_SUBSCRIPTION, async (track, active) => {
        for (;;) {
          const group = await track.recvGroup();
          if (!active() || !group) break;
          try {
            for (;;) {
              const frame = await group.readFrame();
              if (!frame || !active()) break;
              // The relay seals a quiet track with one empty frame; no ops frame is empty.
              if (frame.payload.byteLength === 0) continue;
              stats.opsReceived++;
              try { onFrame(frame.payload, monotonicNow()); } catch (error) { report({type: 'live-frame-error', error}); }
            }
          } catch (error) {
            report({type: 'live-ops-group-error', publisher, error});
          } finally { safeClose(group); }
        }
      }, onEnd);
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
      stats: () => ({...stats, openRecords: records.size}),
      close,
      closed: closedPromise,
    };
  }
  return {connect};
}
