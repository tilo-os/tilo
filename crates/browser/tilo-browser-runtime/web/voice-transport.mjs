/**
 * A call's voice carrier: `moq-lite` over WebTransport at the host's
 * `/moq/voice`, admitted by the grant's token in the carrier preface
 * (web/moq-carrier.mjs), with `@moq/net` used through its public API only.
 *
 * This participation publishes `<call>/<participation>` with one `audio`
 * track and hears every other publisher the host offers under `<call>`: the
 * host re-checks each against the call's current roster, so a participant
 * who joins after admission is heard in place. Objects are tilo-voice/1
 * (web/voice-profile.mjs), as the native client (tilo-moq `client`) sends
 * and expects: each 20 ms object leaves as one datagram; without datagrams,
 * groups of five objects. Both forms are received.
 *
 * Redundancy follows measured loss, hop by hop. A datagram carries
 * up to two earlier objects as well only while this carrier's own QUIC
 * connection reports loss (one at 1%, repairing an isolated loss 20 ms
 * later; two at 5%, repairing a 40 ms burst), and the relay re-bundles each
 * subscriber's downlink by that subscriber's loss the same way (tilo-moq
 * `voice::Redundancy`). With the encoder in DTX, a silent participant sends
 * one comfort-noise update per 400 ms.
 */
import {createMediaQueue} from './voice-buffers.mjs';
import {MOQ_PROTOCOL, MediaAdmissionError, carrierUnsupported, openCarrier} from './moq-carrier.mjs';
import {decodeVoiceBundle, encodeVoiceBundle, encodeVoiceObject} from './voice-profile.mjs';
import {publishPictures, receivePictures} from './voice-video.mjs';

export const VOICE_PATH = '/moq/voice';
export const AUDIO_TRACK = 'audio';
const FRAMES_PER_GROUP = 5;
const FRAME_DURATION_MS = 20;
const MAX_REMOTE_PUBLISHERS = 7;
const DATAGRAM_BUNDLE_BYTES = 1000;
const FRAME_SAMPLES = 960n;
const MAX_REDUNDANCY = 2;
// The loss policy of tilo-moq `voice::Redundancy`; keep the two identical.
const REDUNDANCY_ONE_PERMILLE = 10;
const REDUNDANCY_TWO_PERMILLE = 50;
const LOSS_WINDOW_MS = 1000;
const LOSS_MIN_PACKETS = 20;
const REDUNDANCY_HOLD_MS = 5000;
// A browser whose WebTransport reports no loss counters protects isolated
// losses on its uplink blind; the relay still strips the copy for every
// subscriber whose own downlink is clean.
const UNMEASURED_REDUNDANCY = 1;
const HEX64 = /^[0-9a-f]{64}$/;
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

/** The grant's carrier endpoint: an exact credential-free HTTPS `/moq/voice` URL. */
export function voiceEndpoint(value) {
  const endpoint = new URL(value);
  if (endpoint.protocol !== 'https:' || endpoint.username || endpoint.password
    || endpoint.search || endpoint.hash || endpoint.pathname !== VOICE_PATH) {
    throw new TypeError('voice endpoint must be an exact credential-free HTTPS /moq/voice URL');
  }
  return endpoint;
}

function hexBytes(hex) {
  if (typeof hex !== 'string' || hex.length % 2 || !/^[0-9a-f]*$/.test(hex)) throw new TypeError('expected lowercase hex');
  const bytes = new Uint8Array(hex.length / 2);
  for (let index = 0; index < bytes.length; index++) bytes[index] = parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  return bytes;
}

/**
 * The pins for a development certificate. A browser accepts
 * `serverCertificateHashes` only for loopback or explicit development hosts;
 * a grant without fingerprints is verified against the system roots.
 */
export function certificateHashes(grant) {
  const pins = (grant.fingerprints ?? []).filter(value => HEX64.test(value));
  if (!pins.length) return undefined;
  return pins.map(value => ({algorithm: 'sha-256', value: hexBytes(value)}));
}

/**
 * Earlier objects to add to each datagram, from one hop's cumulative sent
 * and lost packet counts: up within one loss window, down one level after
 * `REDUNDANCY_HOLD_MS` of lower loss.
 */
export function createRedundancy() {
  let level = 0;
  let window;
  let calmSince;
  return {
    get level() { return level; },
    observe(now, sent, lost) {
      if (!window) { window = {start: now, sent, lost}; return level; }
      const packets = sent - window.sent;
      if (now - window.start < LOSS_WINDOW_MS || packets < LOSS_MIN_PACKETS) return level;
      const permille = Math.floor(Math.max(0, lost - window.lost) * 1000 / packets);
      const start = window.start;
      window = {start: now, sent, lost};
      const target = permille >= REDUNDANCY_TWO_PERMILLE ? 2 : permille >= REDUNDANCY_ONE_PERMILLE ? 1 : 0;
      if (target >= level) {
        level = target;
        calmSince = undefined;
      } else {
        calmSince ??= start;
        if (now - calmSince >= REDUNDANCY_HOLD_MS) {
          level--;
          calmSince = level > target ? now : undefined;
        }
      }
      return level;
    },
  };
}

/** Whether `grant` points at a loopback carrier (tests and local relays). */
export const loopback = grant => LOOPBACK.has(new URL(grant.endpoint).hostname);

function validateGrant(grant) {
  if (!grant || !HEX64.test(grant.call) || !HEX64.test(grant.participation)) throw new TypeError('grant names no call or participation');
  const token = hexBytes(grant.token ?? '');
  if (token.byteLength === 0 || token.byteLength > 8192) throw new TypeError('grant token must be 1..8192 bytes');
  const directions = grant.directions ?? {};
  if (typeof directions.publish !== 'boolean' || typeof directions.subscribe !== 'boolean') throw new TypeError('grant directions are required');
  const own = `${grant.call}/${grant.participation}`;
  if (directions.publish && grant.publication !== own) throw new TypeError('a publishing grant names its own namespace');
  return {token, own, directions: {...directions}};
}

/**
 * `library` is `@moq/net` (the bundled media-moq-library.mjs by default);
 * `carrier` opens an admitted WebTransport session (web/moq-carrier.mjs).
 * Tests inject both.
 */
export function createVoiceTransport({library, carrier = openCarrier, clock = {}, onReport = () => {}, datagrams = true} = {}) {
  const monotonicNow = clock.now ?? (() => performance.now());
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (handle => globalThis.clearTimeout(handle));
  const report = value => { try { onReport(value); } catch {} };
  let libraryPromise;
  const load = () => libraryPromise ??= library ? Promise.resolve(library) : import('./media-moq-library.mjs');

  /** Open the carrier `grant` admits. Resolves the session, or rejects when the host denies the token. */
  async function connect({grant, signal}) {
    signal?.throwIfAborted?.();
    const url = voiceEndpoint(grant.endpoint);
    const {token, own, directions} = validateGrant(grant);
    const moq = await load();
    const outgoing = new moq.Origin.Producer();
    const incoming = new moq.Origin.Producer();
    const opened = await carrier({url, grant: token, serverCertificateHashes: certificateHashes(grant), signal});
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
      if (opened.admissionRevoked) throw new MediaAdmissionError();
      throw error;
    }
    if (opened.admissionRevoked) {connection.close(); opened.close(); throw new MediaAdmissionError();}
    if (signal?.aborted) { connection.close(); opened.close(); throw signal.reason; }
    if (connection.version !== MOQ_PROTOCOL || connection.transport !== 'webtransport') {
      connection.close();
      opened.close();
      throw new Error(`voice carrier did not negotiate ${MOQ_PROTOCOL} over WebTransport`);
    }

    let closed = false;
    let broadcast;
    let audioTrack;
    let pictureReader;
    let group;
    let groupFrames = 0;
    let groupTimer;
    let sequence = 0n;
    const recent = [];
    const cleanups = new Set();
    const stats = {mode: 'streams', sent: 0, received: 0, recovered: 0, duplicates: 0, redundancy: 0};
    // Uplink loss, from this carrier's own QUIC connection.
    const uplink = createRedundancy();
    let measured = typeof opened.transport?.getStats === 'function';
    let sampledAt;
    let sampling = false;
    const sampleLoss = () => {
      const at = monotonicNow();
      if (!measured || sampling || (sampledAt !== undefined && at - sampledAt < LOSS_WINDOW_MS)) return;
      sampling = true;
      sampledAt = at;
      Promise.resolve().then(() => opened.transport.getStats()).then(connection => {
        const {packetsSent, packetsLost} = connection ?? {};
        if (!Number.isFinite(packetsSent) || !Number.isFinite(packetsLost)) { measured = false; return; }
        const before = uplink.level;
        uplink.observe(monotonicNow(), packetsSent, packetsLost);
        if (uplink.level !== before) report({type: 'voice-redundancy', level: uplink.level});
      }, () => { measured = false; }).finally(() => { sampling = false; });
    };
    const redundancy = () => Math.min(MAX_REDUNDANCY, measured ? uplink.level : UNMEASURED_REDUNDANCY);
    const safeClose = resource => { try { resource?.close?.(); } catch {} };
    const finishGroup = () => {
      if (groupTimer !== undefined) clearTimer(groupTimer);
      groupTimer = undefined;
      safeClose(group);
      group = undefined;
      groupFrames = 0;
    };
    const close = () => {
      if (closed) return;
      closed = true;
      signal?.removeEventListener?.('abort', close);
      for (const cleanup of [...cleanups]) { try { cleanup(); } catch {} }
      cleanups.clear();
      finishGroup();
      pictureReader?.close();
      safeClose(audioTrack);
      safeClose(broadcast);
      safeClose(connection);
      safeClose(outgoing);
      safeClose(incoming);
      opened.close();
    };
    signal?.addEventListener?.('abort', close, {once: true});
    const closedPromise = Promise.race([Promise.resolve(connection.closed), opened.revoked]).then(() => close(), () => close());

    let useDatagrams = false;
    try {
      if (directions.publish) {
        broadcast = outgoing.createBroadcast(moq.Path.from(own));
        // Late audio is dropped, never queued: 100 ms retention.
        audioTrack = broadcast.createTrack(AUDIO_TRACK, {timescale: moq.Time.Timescale.MICRO, maxAge: 100});
        broadcast.announce();
        useDatagrams = datagrams === true && typeof audioTrack.appendDatagram === 'function';
        if (useDatagrams) stats.mode = 'datagrams';
      }
    } catch (error) {
      close();
      throw error;
    }

    /** Publish one encoded 20 ms frame `{timestamp (48 kHz BigInt), duration, payload}`. */
    function publish(frame) {
      if (closed || !audioTrack) return false;
      const bytes = encodeVoiceObject({sequence, timestamp: frame.timestamp, duration: frame.duration, payload: frame.payload});
      const at = new moq.Time.Timestamp(Number(frame.timestamp * 1_000_000n / 48_000n), moq.Time.Timescale.MICRO);
      if (useDatagrams) {
        sampleLoss();
        const level = redundancy();
        stats.redundancy = level;
        const objects = [bytes];
        let size = bytes.byteLength;
        for (let index = recent.length - 1; index >= 0 && objects.length <= level; index--) {
          const earlier = recent[index];
          // Only the frames just before: one from before a DTX pause would
          // arrive long after its playout.
          if (frame.timestamp - earlier.timestamp > BigInt(MAX_REDUNDANCY + 1) * FRAME_SAMPLES) break;
          if (size + earlier.bytes.byteLength > DATAGRAM_BUNDLE_BYTES) break;
          objects.push(earlier.bytes);
          size += earlier.bytes.byteLength;
        }
        recent.push({bytes, timestamp: frame.timestamp});
        if (recent.length > MAX_REDUNDANCY) recent.shift();
        audioTrack.appendDatagram(at, encodeVoiceBundle(objects));
      } else {
        // Group n carries objects 5n..5n+4, as the native client sends them.
        if (!group || sequence % BigInt(FRAMES_PER_GROUP) === 0n) {
          finishGroup();
          group = audioTrack.appendGroup();
          groupTimer = setTimer(finishGroup, FRAMES_PER_GROUP * FRAME_DURATION_MS * 2);
        }
        group.writeFrame({payload: bytes, timestamp: at});
        if (++groupFrames === FRAMES_PER_GROUP) finishGroup();
      }
      sequence++;
      stats.sent++;
      return true;
    }

    /**
     * Hear the call: `onFrame({publisher, sequence, timestamp, duration,
     * payload})` for each object of each other publisher, oldest first;
     * `onPublisher(publisher, active)` as publishers come and go.
     */
    function subscribe({onFrame, onPublisher = () => {}}) {
      if (closed) throw new Error('voice carrier closed');
      if (!directions.subscribe) return () => {};
      let active = true;
      const callPath = moq.Path.from(grant.call);
      const announcements = incoming.announced(moq.Path.Pattern.subtree(callPath));
      const remotes = new Map();
      const deliver = (remote, object, recovered) => {
        const queued = remote.queue.push({key: object.sequence.toString(), receivedAtMs: monotonicNow(), durationMs: FRAME_DURATION_MS, bytes: object.payload});
        if (!queued.accepted) {
          if (queued.reason === 'duplicate') stats.duplicates++;
          return;
        }
        remote.queue.drain();
        stats.received++;
        if (recovered) stats.recovered++;
        try { onFrame({publisher: remote.publisher, ...object}); } catch (error) { report({type: 'voice-frame-error', error}); }
      };
      const stop = (remote, notify = true) => {
        if (!remote.active) return;
        remote.active = false;
        remotes.delete(remote.publisher);
        remote.finish();
        safeClose(remote.track);
        safeClose(remote.request);
        if (notify) { try { onPublisher(remote.publisher, false); } catch {} }
      };
      const pumpDatagrams = async remote => {
        try {
          for (;;) {
            const datagram = await remote.track.recvDatagram();
            if (!datagram || !remote.active) return;
            let objects;
            try { objects = decodeVoiceBundle(datagram.payload); } catch { report({type: 'voice-receive-drop', reason: 'invalid-object'}); continue; }
            for (let index = objects.length - 1; index >= 0; index--) deliver(remote, objects[index], index > 0);
          }
        } catch (error) {
          if (remote.active) report({type: 'voice-subscription-error', error});
        }
      };
      const pumpGroups = async remote => {
        try {
          for (;;) {
            let next = await remote.track.recvGroup();
            if (!next || !remote.active) return;
            // Late audio is worthless: skip to the newest buffered group.
            for (;;) { const newer = remote.track.tryRecvGroup(); if (!newer) break; safeClose(next); next = newer; }
            try {
              for (let frames = 0; frames < FRAMES_PER_GROUP; frames++) {
                const frame = await next.readFrame();
                if (!frame || !remote.active) break;
                let objects;
                try { objects = decodeVoiceBundle(frame.payload); } catch { report({type: 'voice-receive-drop', reason: 'invalid-object'}); continue; }
                for (let index = objects.length - 1; index >= 0; index--) deliver(remote, objects[index], index > 0);
              }
            } catch {
              // One group's stream was reset (the relay dropped it for a
              // newer one, or this reader fell behind its cache): that group
              // is lost, the track is not. Its end is recvGroup's to say.
              report({type: 'voice-receive-drop', reason: 'group-reset'});
            } finally { safeClose(next); }
          }
        } catch (error) {
          if (remote.active) report({type: 'voice-subscription-error', error});
        }
      };
      const start = publisher => {
        if (!active || remotes.has(publisher)) return;
        if (remotes.size >= MAX_REMOTE_PUBLISHERS) { report({type: 'voice-publisher-limit'}); return; }
        let finish;
        const finished = new Promise(resolve => {finish = resolve;});
        const sequenceWindow = () => createMediaQueue({maxBytes: 10 * 4096, maxAgeMs: 200, maxFrames: 10, maxDurationMs: 200, maxDedupEntries: 32, now: monotonicNow});
        const remote = {
          publisher,
          finish,
          active: true,
          request: undefined,
          track: undefined,
          queue: sequenceWindow(),
        };
        remotes.set(publisher, remote);
        try { onPublisher(publisher, true); } catch {}
        void (async () => {
          try {
            // Own the request before awaiting resolution so retract, leave
            // and grant replacement can release it immediately.
            remote.request = incoming.request(moq.Path.from(grant.call, publisher));
            // The request follows the path: a renewed carrier's republish
            // takes it over without the announcement ever being retracted,
            // and the broadcast this subscription read then ends. Hear the
            // broadcast that took the path, from its own sequence window,
            // until the publisher is retracted or the call is left.
            let current;
            while (remote.active) {
              let broadcast;
              while (remote.active && (!(broadcast = remote.request.active.peek()) || broadcast === current)) {
                await Promise.race([remote.request.active.changed(), finished]);
              }
              if (!remote.active) return;
              if (current) remote.queue = sequenceWindow();
              current = broadcast;
              remote.track = broadcast.track(AUDIO_TRACK).subscribe({priority: 0, maxAge: 200});
              await Promise.all([pumpDatagrams(remote), pumpGroups(remote)]);
              safeClose(remote.track);
            }
          } catch (error) {
            if (remote.active) report({type: 'voice-subscription-error', error});
          } finally {
            stop(remote);
          }
        })();
      };
      const cleanup = () => {
        if (!active) return;
        active = false;
        cleanups.delete(cleanup);
        safeClose(announcements);
        for (const remote of [...remotes.values()]) stop(remote);
      };
      cleanups.add(cleanup);
      void (async () => {
        try {
          while (active) {
            const event = await announcements.next();
            if (!event) break;
            const publisher = moq.Path.stripPrefix(callPath, event.prefix);
            if (!publisher || !HEX64.test(publisher) || publisher === grant.participation) continue;
            // A renewed carrier may re-announce before the old receive
            // loops finish. Replace its request and sequence window now.
            const previous = remotes.get(publisher);
            if (previous) stop(previous);
            if (event.kind !== 'retracted') start(publisher);
          }
        } catch (error) {
          if (active) report({type: 'voice-announcement-error', error});
        } finally {
          cleanup();
        }
      })();
      return cleanup;
    }

    /**
     * Publish `stream`'s pictures as `source` on this carrier's broadcast
     * (web/voice-video.mjs `publishPictures`); returns `{stop}`.
     */
    function publishPicturesOf({source, stream, environment, ownLayer, own, onEnded}) {
      if (closed || !broadcast) throw new Error('this carrier does not publish');
      return publishPictures({moq, broadcast, source, stream, environment, ownLayer, own, onEnded, report});
    }

    /** Read exactly the picture tracks `wanted` (`[{publisher, source,
     * layer}]`) into `deliver(publisher, source, bytes, keyframe)`. */
    function showPictures(wanted, deliver) {
      if (closed || !directions.subscribe) return;
      pictureReader ??= receivePictures({moq, incoming, call: grant.call, deliver, report});
      pictureReader.show(wanted);
    }

    return {
      participation: grant.participation,
      directions,
      publish,
      subscribe,
      publishPictures: publishPicturesOf,
      showPictures,
      stats: () => ({...stats}),
      close,
      closed: closedPromise,
      authorityLost: opened.authorityLost,
    };
  }
  /** Why this browser cannot open the carrier, or null when it can. */
  const unsupported = () => (carrier === openCarrier ? carrierUnsupported() : null);
  return {connect, unsupported};
}
