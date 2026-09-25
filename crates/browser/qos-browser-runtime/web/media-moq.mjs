import {createMediaQueue} from './media-buffers.mjs';
import {decodeVoiceBundle, decodeVoiceObject, encodeVoiceBundle, encodeVoiceObject} from './media-voice-profile.mjs';

const DRAFT_VERSION = 'moq-transport-21';
const DRAFT_PROTOCOL = 'moqt-21';
const AUDIO_TRACK = 'audio';
const FRAME_DURATION_MS = 20;
const MAX_REMOTE_PUBLISHERS = 7;
const MAX_AUTHORIZATION_BYTES = 8192;
const MAX_REFRESH_MS = 60_000;
const OPAQUE_COMPONENT = /^[0-9a-f]{64}$/;
// Datagram delivery: each 20 ms object leaves as one Draft 21 OBJECT_DATAGRAM
// (no retransmission, no head-of-line blocking) carrying the newest object plus
// REDUNDANCY earlier objects, so an isolated loss is repaired by the next
// datagram 20 ms later instead of a QUIC retransmission one RTT later. The
// depth rises to 2 while loss is sustained: from transport stats where the
// browser reports them, otherwise from the primary-datagram loss this client
// observes on its own receive path (edge access links carry both directions).
const DATAGRAM_BUNDLE_BYTES = 1000;
const REDUNDANCY_MIN = 1;
const REDUNDANCY_MAX = 2;
const LOSS_SAMPLE_MS = 2_000;
const LOSS_HIGH = 0.03;
const LOSS_LOW = 0.01;
const STATS_REPORT_MS = 5_000;

function validateEndpoint(value) {
  const endpoint = new URL(value instanceof URL ? value.href : value);
  if (endpoint.protocol !== 'https:'
    || endpoint.username !== ''
    || endpoint.password !== ''
    || endpoint.search !== ''
    || endpoint.hash !== ''
    || endpoint.pathname !== '/moq/v21/voice') {
    throw new TypeError('media endpoint must be an exact credential-free HTTPS URL');
  }
  return endpoint;
}

function validateCertificateHashes(values) {
  if (values === undefined) return undefined;
  if (!Array.isArray(values) || values.length === 0 || values.length > 8) {
    throw new TypeError('server certificate hashes must be a non-empty bounded array');
  }
  return values.map(hash => {
    if (!hash || (hash.algorithm ?? 'sha-256') !== 'sha-256'
      || !(hash.value instanceof Uint8Array) || hash.value.byteLength !== 32) {
      throw new TypeError('each server certificate hash must contain exactly 32 SHA-256 bytes');
    }
    return {algorithm: 'sha-256', value: new Uint8Array(hash.value)};
  });
}

function validateAuthorization(value) {
  if (!value || !(value.authorization instanceof Uint8Array)
    || value.authorization.byteLength === 0
    || value.authorization.byteLength > MAX_AUTHORIZATION_BYTES) {
    throw new TypeError('authorization must contain 1 to 8192 opaque bytes');
  }
  if (!Number.isInteger(value.carrierHandle) || value.carrierHandle <= 0 || value.carrierHandle > 0xffffffff) {
    throw new TypeError('carrier handle must be an opaque positive Rust u32');
  }
  if (typeof value.publishNamespace !== 'string') {
    throw new TypeError('publish namespace is required');
  }
  const parts = value.publishNamespace.split('/');
  if (parts.length !== 2 || !parts.every(part => OPAQUE_COMPONENT.test(part))) {
    throw new TypeError('publish namespace must contain exactly two opaque components');
  }
  if (!OPAQUE_COMPONENT.test(value.subscribePrefix) || value.subscribePrefix !== parts[0]) {
    throw new TypeError('subscribe prefix must be the exact opaque call component');
  }
  if (!Array.isArray(value.subscribeNamespaces) || value.subscribeNamespaces.length > MAX_REMOTE_PUBLISHERS) throw new TypeError('exact remote namespace inventory is required');
  let previous='';
  for(const namespace of value.subscribeNamespaces){
    const remote=typeof namespace==='string'?namespace.split('/'):[];
    if(remote.length!==2||remote[0]!==parts[0]||!OPAQUE_COMPONENT.test(remote[1])||namespace===value.publishNamespace||namespace<=previous) throw new TypeError('remote namespace inventory must be sorted unique same-call and exclude self');
    previous=namespace;
  }
  const directions = value.directions;
  if (!directions || typeof directions.publish !== 'boolean'
    || typeof directions.subscribe !== 'boolean'
    || (!directions.publish && !directions.subscribe)) {
    throw new TypeError('directions must authorize publish or subscribe');
  }
  return {
    authorization: new Uint8Array(value.authorization),
    carrierHandle: value.carrierHandle,
    subscribeNamespaces: new Set(value.subscribeNamespaces),
    publishNamespace: value.publishNamespace,
    subscribePrefix: value.subscribePrefix,
    directions: {publish: directions.publish, subscribe: directions.subscribe},
  };
}

export function createMoqTransport({
  url,
  serverCertificateHashes,
  authorize,
  refreshAuthorization,
  library,
  clock = {},
  onReport = () => {},
  datagrams = true,
}) {
  if (typeof authorize !== 'function' || typeof refreshAuthorization !== 'function') {
    throw new TypeError('authorize and refreshAuthorization functions are required');
  }
  if (typeof onReport !== 'function') throw new TypeError('onReport must be a function');
  const endpoint = validateEndpoint(url);
  const certificateHashes = validateCertificateHashes(serverCertificateHashes);
  const defaultMonotonicNow = typeof globalThis.performance?.now === 'function'
    ? () => globalThis.performance.now()
    : undefined;
  const monotonicNow = clock.monotonicNow ?? defaultMonotonicNow;
  const wallNow = clock.wallNow ?? Date.now;
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (timer => globalThis.clearTimeout(timer));
  if (typeof monotonicNow !== 'function' || typeof wallNow !== 'function'
    || typeof setTimer !== 'function' || typeof clearTimer !== 'function') {
    throw new TypeError('clock functions are invalid');
  }
  const report = value => {
    try { onReport(value); } catch {}
  };
  const libraryPromise = library ? Promise.resolve(library) : import('./media-moq-library.mjs');

  async function connect({grant, signal}) {
    signal?.throwIfAborted?.();
    const authorized = validateAuthorization(await authorize({grant, signal}));
    signal?.throwIfAborted?.();
    const moq = await libraryPromise;
    const webtransport = {protocols: [DRAFT_PROTOCOL]};
    if (certificateHashes) webtransport.serverCertificateHashes = certificateHashes;
    const connection = await moq.Connection.connect(endpoint, {
      authorization: authorized.authorization,
      websocket: {enabled: false},
      webtransport,
      signal,
    });
    if (signal?.aborted) {
      connection.close();
      throw signal.reason;
    }
    if (connection.version !== DRAFT_VERSION || connection.transport !== 'webtransport') {
      connection.close();
      throw new Error('MoQ connection did not negotiate moq-transport-21 over WebTransport');
    }

    let closed = false;
    let outgoingGroup;
    let outgoingGroupFrames = 0;
    let outgoingGroupStartedAt;
    let outgoingGroupTimer;
    let nextSequence = 0n;
    let subscriptionCleanup;
    let refreshGeneration = 0;
    let broadcast;
    let audioTrack;
    let useDatagrams = false;
    let redundancy = REDUNDANCY_MIN;
    const recentObjects = [];
    let lossSampling = false;
    let lossSampledAt;
    let statsReportedAt;
    let lossBaseline;
    const receiveLoss = {expected: 0, received: 0};
    const transportStats = {
      receiveLoss: null,
      mode: 'streams',
      sentDatagrams: 0,
      redundancy: REDUNDANCY_MIN,
      uplinkLoss: null,
      receivedObjects: 0,
      recoveredObjects: 0,
      duplicateObjects: 0,
      missingObjects: 0,
    };

    const safeClose = resource => {
      try { resource?.close?.(); } catch {}
    };
    const close = () => {
      if (closed) return;
      closed = true;
      refreshGeneration++;
      signal?.removeEventListener?.('abort', close);
      if (outgoingGroupTimer !== undefined) clearTimer(outgoingGroupTimer);
      outgoingGroupTimer = undefined;
      try { subscriptionCleanup?.(); } catch {}
      subscriptionCleanup = undefined;
      safeClose(outgoingGroup);
      outgoingGroup = undefined;
      safeClose(audioTrack);
      safeClose(broadcast);
      safeClose(connection);
    };
    signal?.addEventListener?.('abort', close, {once: true});
    void Promise.resolve(connection.closed).then(close, close);

    try {
      if (authorized.directions.publish) {
        broadcast = new moq.Broadcast.Producer();
        audioTrack = broadcast.createTrack(AUDIO_TRACK, {
          timescale: moq.Time.Timescale.MICRO,
          latencyMax: 100,
          ordered: false,
        }, {
          maxGroups: 1,
          maxFrames: 5,
          maxAgeMs: 100,
        });
        connection.publish(moq.Path.from(authorized.publishNamespace), broadcast);
        useDatagrams = datagrams === true && connection.transport === 'webtransport'
          && typeof audioTrack.appendDatagram === 'function';
        if (useDatagrams) transportStats.mode = 'datagrams';
      }
    } catch (error) {
      close();
      throw error;
    }

    // Redundancy depth follows transport-reported uplink loss where the
    // browser exposes it (Chromium 153 has no WebTransport getStats), else the
    // primary-datagram loss seen on this client's receive path; the minimum
    // depth always covers isolated losses on either leg. Sampled from the 20 ms
    // media cadence rather than a timer, so an idle or closed session owns no
    // timers. recoveredObjects counts objects first delivered from redundancy
    // (a repaired loss or a reordered primary).
    const sampleLoss = async () => {
      if (closed || lossSampling) return;
      lossSampling = true;
      try {
        const stats = await connection.stats?.();
        const sent = stats?.packetsSent;
        const lost = stats?.packetsLost;
        if (Number.isFinite(sent) && Number.isFinite(lost)) {
          if (lossBaseline && sent > lossBaseline.sent) {
            const ratio = Math.max(0, lost - lossBaseline.lost) / (sent - lossBaseline.sent);
            transportStats.uplinkLoss = Math.round(ratio * 10_000) / 10_000;
            if (ratio >= LOSS_HIGH) redundancy = REDUNDANCY_MAX;
            else if (ratio <= LOSS_LOW) redundancy = REDUNDANCY_MIN;
            transportStats.redundancy = redundancy;
          }
          lossBaseline = {sent, lost};
        }
      } catch {} finally {
        lossSampling = false;
      }
    };
    const periodic = () => {
      const current = monotonicNow();
      if (useDatagrams && (lossSampledAt === undefined || current - lossSampledAt >= LOSS_SAMPLE_MS)) {
        lossSampledAt = current;
        if (receiveLoss.expected >= 25) {
          const ratio = Math.max(0, receiveLoss.expected - receiveLoss.received) / receiveLoss.expected;
          transportStats.receiveLoss = Math.round(ratio * 10_000) / 10_000;
          if (transportStats.uplinkLoss === null) {
            if (ratio >= LOSS_HIGH) redundancy = REDUNDANCY_MAX;
            else if (ratio <= LOSS_LOW) redundancy = REDUNDANCY_MIN;
            transportStats.redundancy = redundancy;
          }
          receiveLoss.expected = 0;
          receiveLoss.received = 0;
        }
        void sampleLoss();
      }
      if (statsReportedAt === undefined) statsReportedAt = current;
      else if (current - statsReportedAt >= STATS_REPORT_MS) {
        statsReportedAt = current;
        report({type: 'media-transport-stats', ...transportStats});
      }
    };

    const finishOutgoingGroup = reason => {
      if (!outgoingGroup) return;
      const group = outgoingGroup;
      const frames = outgoingGroupFrames;
      outgoingGroup = undefined;
      outgoingGroupFrames = 0;
      outgoingGroupStartedAt = undefined;
      if (outgoingGroupTimer !== undefined) clearTimer(outgoingGroupTimer);
      outgoingGroupTimer = undefined;
      safeClose(group);
      if (reason === 'deadline') report({type: 'media-group-expired', frames});
    };

    const publish = frame => {
      if (closed) throw new Error('MoQ media session is closed');
      if (!audioTrack) throw new Error('MoQ media session is not authorized to publish');
      if (typeof frame?.timestamp !== 'bigint' || frame.timestamp < 0n) {
        throw new RangeError('frame timestamp must fit the qualified MoQ timestamp range');
      }
      const timestampMicros = frame.timestamp * 1_000_000n / 48_000n;
      if (timestampMicros > BigInt(Number.MAX_SAFE_INTEGER)) {
        throw new RangeError('frame timestamp must fit the qualified MoQ timestamp range');
      }
      const sequence = nextSequence;
      const bytes = encodeVoiceObject({
        sequence,
        timestamp: frame.timestamp,
        duration: frame.duration,
        payload: frame.payload,
      });
      if (useDatagrams) {
        const objects = [bytes];
        let size = bytes.byteLength;
        for (let index = recentObjects.length - 1; index >= 0 && objects.length <= redundancy; index--) {
          if (size + recentObjects[index].byteLength > DATAGRAM_BUNDLE_BYTES) break;
          objects.push(recentObjects[index]);
          size += recentObjects[index].byteLength;
        }
        recentObjects.push(bytes);
        if (recentObjects.length > REDUNDANCY_MAX) recentObjects.shift();
        audioTrack.appendDatagram(
          new moq.Time.Timestamp(Number(timestampMicros), moq.Time.Timescale.MICRO),
          encodeVoiceBundle(objects),
        );
        nextSequence++;
        transportStats.sentDatagrams++;
        periodic();
        return true;
      }
      const current = monotonicNow();
      if (outgoingGroup && current - outgoingGroupStartedAt >= 100) {
        finishOutgoingGroup('deadline');
      }
      if (!outgoingGroup) {
        outgoingGroup = audioTrack.appendGroup();
        outgoingGroupFrames = 0;
        outgoingGroupStartedAt = current;
        outgoingGroupTimer = setTimer(() => finishOutgoingGroup('deadline'), 100);
      }
      outgoingGroup.writeFrame({
        payload: bytes,
        timestamp: new moq.Time.Timestamp(
          Number(frame.timestamp * 1_000_000n / 48_000n),
          moq.Time.Timescale.MICRO,
        ),
      });
      nextSequence++;
      outgoingGroupFrames++;
      if (outgoingGroupFrames === 5) {
        finishOutgoingGroup('complete');
      }
      return true;
    };

    const subscribe = async ({onFrame, onPublisherEnded, signal: subscriptionSignal}) => {
      if (closed) throw new Error('MoQ media session is closed');
      if (!authorized.directions.subscribe) {
        throw new Error('MoQ media session is not authorized to subscribe');
      }
      if (subscriptionCleanup) throw new Error('MoQ media session already has a subscription');
      if (typeof onFrame !== 'function' || typeof onPublisherEnded !== 'function') {
        throw new TypeError('onFrame and onPublisherEnded functions are required');
      }
      subscriptionSignal?.throwIfAborted?.();

      let active = true;
      let generation = 0;
      const remotes = new Map();
      const announcements = connection.announced(moq.Path.from(authorized.subscribePrefix));
      const ownPublisherId = authorized.publishNamespace.split('/')[1];

      const notifyEnded = publisherId => {
        try { onPublisherEnded(publisherId); } catch (error) {
          report({type: 'media-publisher-ended-error', error});
        }
      };
      const stopRemote = (remote, notify = true) => {
        if (!remote.active) return;
        remote.active = false;
        if (remotes.get(remote.publisherId) === remote) remotes.delete(remote.publisherId);
        remote.queue.clear();
        try { remote.track.close(); } catch {}
        try { remote.broadcast.close(); } catch {}
        if (notify) notifyEnded(remote.publisherId);
      };
      const pump = async remote => {
        try {
          for (;;) {
            let group = await remote.track.recvGroup();
            if (!group) break;
            for (;;) {
              const newer = remote.track.tryRecvGroup();
              if (!newer) break;
              safeClose(group);
              group = newer;
              report({type: 'media-receive-drop', reason: 'stale-group', publisherId: remote.publisherId});
            }
            try {
              const groupDeadline = monotonicNow() + 200;
              for (let groupFrames = 0; groupFrames < 5;) {
                const remaining = groupDeadline - monotonicNow();
                if (remaining <= 0) {
                  report({type: 'media-receive-drop', reason: 'group-deadline', publisherId: remote.publisherId});
                  break;
                }
                const timeout = Symbol('group-deadline');
                let timer;
                const frame = await Promise.race([
                  group.readFrame(),
                  new Promise(resolve => { timer = setTimer(() => resolve(timeout), remaining); }),
                ]).finally(() => clearTimer(timer));
                if (frame === timeout) {
                  report({type: 'media-receive-drop', reason: 'group-deadline', publisherId: remote.publisherId});
                  break;
                }
                if (!frame) break;
                groupFrames++;
                if (!active || !remote.active || remote.generation !== generation
                  || remotes.get(remote.publisherId) !== remote) return;
                let decoded;
                try {
                  decoded = decodeVoiceObject(frame.payload);
                } catch (error) {
                  report({type: 'media-receive-drop', reason: 'invalid-object', publisherId: remote.publisherId, error});
                  continue;
                }
                const receivedAtMs = monotonicNow();
                const queued = remote.queue.push({
                  key: decoded.sequence.toString(),
                  receivedAtMs,
                  durationMs: FRAME_DURATION_MS,
                  bytes: frame.payload,
                });
                if (!queued.accepted) {
                  report({type: 'media-receive-drop', reason: queued.reason, publisherId: remote.publisherId});
                  continue;
                }
                for (const entry of remote.queue.drain()) {
                  const object = decodeVoiceObject(entry.bytes);
                  try {
                    const result = onFrame({publisherId: remote.publisherId, ...object});
                    if (result?.then) void result.catch(error => {
                      if (active && remote.active && remote.generation === generation) {
                        report({type: 'media-frame-error', error});
                      }
                    });
                  } catch (error) {
                    report({type: 'media-frame-error', error});
                  }
                }
              }
            } finally {
              group.close();
            }
          }
        } catch (error) {
          if (active && remote.active) {
            report({type: 'media-subscription-error', publisherId: remote.publisherId, error});
          }
        } finally {
          if (active && remote.active && remote.generation === generation) stopRemote(remote);
        }
      };
      const deliverDatagramObject = (remote, object, recovered) => {
        const queued = remote.queue.push({
          key: object.sequence.toString(),
          receivedAtMs: monotonicNow(),
          durationMs: FRAME_DURATION_MS,
          bytes: encodeVoiceObject(object),
        });
        if (!queued.accepted) {
          if (queued.reason === 'duplicate') transportStats.duplicateObjects++;
          else report({type: 'media-receive-drop', reason: queued.reason, publisherId: remote.publisherId});
          return;
        }
        transportStats.receivedObjects++;
        if (recovered) transportStats.recoveredObjects++;
        periodic();
        if (remote.highestSequence === undefined) {
          remote.highestSequence = object.sequence;
        } else if (object.sequence > remote.highestSequence) {
          const gap = object.sequence - remote.highestSequence - 1n;
          if (gap > 0n && gap < 1_000n) transportStats.missingObjects += Number(gap);
          remote.highestSequence = object.sequence;
        } else if (transportStats.missingObjects > 0) {
          transportStats.missingObjects--;
        }
        for (const entry of remote.queue.drain()) {
          const decoded = decodeVoiceObject(entry.bytes);
          try {
            const result = onFrame({publisherId: remote.publisherId, ...decoded});
            if (result?.then) void result.catch(error => {
              if (active && remote.active && remote.generation === generation) {
                report({type: 'media-frame-error', error});
              }
            });
          } catch (error) {
            report({type: 'media-frame-error', error});
          }
        }
      };
      const pumpDatagrams = async remote => {
        if (typeof remote.track.recvDatagram !== 'function') return;
        try {
          for (;;) {
            const datagram = await remote.track.recvDatagram();
            if (!datagram) return;
            if (!active || !remote.active || remote.generation !== generation
              || remotes.get(remote.publisherId) !== remote) return;
            let objects;
            try {
              objects = decodeVoiceBundle(datagram.payload);
            } catch (error) {
              report({type: 'media-receive-drop', reason: 'invalid-object', publisherId: remote.publisherId, error});
              continue;
            }
            // Primary-datagram loss on this receive path (before repair).
            const primary = objects[0].sequence;
            if (remote.highestPrimary === undefined || primary > remote.highestPrimary) {
              const step = remote.highestPrimary === undefined ? 1n : primary - remote.highestPrimary;
              if (step > 0n && step < 50n) {
                receiveLoss.expected += Number(step);
                receiveLoss.received++;
              }
              remote.highestPrimary = primary;
            } else if (receiveLoss.expected > receiveLoss.received) {
              // A reordered primary: it was counted missing when the gap opened.
              receiveLoss.received++;
            }
            // Oldest first, so a recovered frame reaches the decoder before the
            // newer frame that carried it.
            for (let index = objects.length - 1; index >= 0; index--) {
              deliverDatagramObject(remote, objects[index], index > 0);
            }
          }
        } catch (error) {
          if (active && remote.active) {
            report({type: 'media-subscription-error', publisherId: remote.publisherId, error});
          }
        }
      };
      const startRemote = publisherId => {
        if (!active || remotes.has(publisherId)) return;
        if (remotes.size >= MAX_REMOTE_PUBLISHERS) {
          report({type: 'media-publisher-limit'});
          return;
        }
        let remoteBroadcast;
        let remoteTrack;
        try {
          remoteBroadcast = connection.consume(moq.Path.from(authorized.subscribePrefix, publisherId));
          remoteTrack = remoteBroadcast.track(AUDIO_TRACK).subscribe({
            priority: 0,
            ordered: false,
            latencyMax: 200,
            maxGroups: 2,
            maxFrames: 10,
            maxAgeMs: 200,
          });
        } catch (error) {
          try { remoteTrack?.close(); } catch {}
          try { remoteBroadcast?.close(); } catch {}
          report({type: 'media-subscription-error', publisherId, error});
          notifyEnded(publisherId);
          return;
        }
        const remote = {
          publisherId,
          broadcast: remoteBroadcast,
          track: remoteTrack,
          generation,
          active: true,
          queue: createMediaQueue({
            maxBytes: 10 * 4096,
            maxAgeMs: 200,
            maxFrames: 10,
            maxDurationMs: 200,
            maxDedupEntries: 32,
            now: monotonicNow,
          }),
        };
        remotes.set(publisherId, remote);
        void pump(remote);
        void pumpDatagrams(remote);
      };

      const cleanup = () => {
        if (!active) return;
        active = false;
        generation++;
        subscriptionSignal?.removeEventListener?.('abort', cleanup);
        try { announcements.close(); } catch {}
        for (const remote of [...remotes.values()]) stopRemote(remote);
        remotes.clear();
        if (subscriptionCleanup === cleanup) subscriptionCleanup = undefined;
      };
      subscriptionCleanup = cleanup;
      subscriptionSignal?.addEventListener?.('abort', cleanup, {once: true});
      void (async () => {
        try {
          while (active) {
            const event = await announcements.next();
            if (!event) break;
            if (!OPAQUE_COMPONENT.test(event.path) || event.path === ownPublisherId) continue;
            // The host relay scopes this session to its own call and
            // re-authorizes every offered namespace, so a publisher that
            // joins after admission is heard in place (bounded by startRemote).
            if (event.active === true) startRemote(event.path);
            else if (event.active === false) {
              const remote = remotes.get(event.path);
              if (remote) stopRemote(remote);
            }
          }
        } catch (error) {
          if (active) report({type: 'media-announcement-error', error});
        } finally {
          if (active) cleanup();
        }
      })();
      return cleanup;
    };

    const refreshSessionAuthorization = async ({grant, signal: refreshSignal} = {}) => {
      if (closed) throw new Error('MoQ media session is closed');
      refreshSignal?.throwIfAborted?.();
      const generation = ++refreshGeneration;
      const result = await refreshAuthorization({
        grant,
        carrierHandle: authorized.carrierHandle,
        signal: refreshSignal,
      });
      if (refreshSignal?.aborted) throw refreshSignal.reason;
      if (closed || generation !== refreshGeneration) {
        throw new Error('stale authorization refresh result for closed media session');
      }
      const current = wallNow();
      if (!result || result.carrierHandle !== authorized.carrierHandle) {
        throw new Error('authorization refresh did not preserve the host-verified session binding');
      }
      if (!Number.isFinite(current)
        || !Number.isFinite(result.expiresAtMs)
        || result.expiresAtMs <= current
        || result.expiresAtMs > current + MAX_REFRESH_MS) {
        throw new RangeError('authorization refresh expiry must be in the next 60 seconds');
      }
      return {expiresAtMs: result.expiresAtMs};
    };

    const session = {
      stats: () => ({...transportStats}),
      publish,
      subscribe,
      close,
      closed: connection.closed,
      refreshAuthorization: refreshSessionAuthorization,
    };
    return session;
  }

  return {connect};
}
