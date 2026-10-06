/** A bounded, deduplicating receive queue for one remote publisher. */
const MAX_KEY_LENGTH = 256;

export function createMediaQueue({
  maxBytes,
  maxAgeMs,
  maxFrames = 10,
  maxDurationMs = 200,
  maxDedupEntries = 32,
  now,
}) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0) throw new RangeError('maxBytes must be a positive safe integer');
  if (!Number.isFinite(maxAgeMs) || maxAgeMs <= 0) throw new RangeError('maxAgeMs must be positive and finite');
  if (!Number.isSafeInteger(maxFrames) || maxFrames <= 0) throw new RangeError('maxFrames must be a positive safe integer');
  if (!Number.isFinite(maxDurationMs) || maxDurationMs <= 0) throw new RangeError('maxDurationMs must be positive and finite');
  if (!Number.isSafeInteger(maxDedupEntries) || maxDedupEntries < maxFrames) {
    throw new RangeError('maxDedupEntries must be a safe integer at least as large as maxFrames');
  }
  if (typeof now !== 'function') throw new TypeError('now must be a monotonic clock function');

  const entries = [];
  const identities = new Map();
  let lastNow;
  let queuedBytes = 0;
  let queuedDurationMs = 0;
  let expired = 0;
  let overflow = 0;
  let duplicates = 0;

  function readNow() {
    const current = now();
    if (!Number.isFinite(current)) throw new RangeError('now returned an invalid clock value');
    if (lastNow !== undefined && current < lastNow) throw new RangeError('monotonic clock regressed');
    lastNow = current;
    return current;
  }

  const isExpired = (receivedAtMs, current) => current - receivedAtMs >= maxAgeMs;

  function expireAt(current) {
    for (let index = entries.length - 1; index >= 0; index--) {
      const queued = entries[index];
      if (!isExpired(queued.entry.receivedAtMs, current)) continue;
      entries.splice(index, 1);
      queuedBytes -= queued.size;
      queuedDurationMs -= queued.entry.durationMs;
      expired++;
    }
    for (const [key, identity] of identities) {
      if (!isExpired(identity.receivedAtMs, current)) continue;
      identities.delete(key);
    }
  }

  function push({key, receivedAtMs, durationMs, bytes}) {
    const validKey = (typeof key === 'string' && key.length > 0 && key.length <= MAX_KEY_LENGTH)
      || (typeof key === 'number' && Number.isSafeInteger(key) && key >= 0);
    if (!validKey) throw new TypeError(`key must be a non-empty string of at most ${MAX_KEY_LENGTH} characters or a non-negative safe integer`);
    if (!(bytes instanceof Uint8Array)) throw new TypeError('bytes must be a Uint8Array');
    if (bytes.byteLength === 0) throw new RangeError('bytes must not be empty');
    if (!Number.isFinite(receivedAtMs)) throw new RangeError('receivedAtMs must be finite');
    if (!Number.isFinite(durationMs) || durationMs <= 0) throw new RangeError('durationMs must be positive and finite');

    const current = readNow();
    if (receivedAtMs > current) throw new RangeError('receivedAtMs is in the future');
    expireAt(current);

    if (isExpired(receivedAtMs, current)) {
      expired++;
      return {accepted: false, reason: 'expired'};
    }
    if (identities.has(key)) {
      duplicates++;
      return {accepted: false, reason: 'duplicate'};
    }
    if (bytes.byteLength > maxBytes || durationMs > maxDurationMs) {
      overflow++;
      return {accepted: false, reason: 'overflow'};
    }

    while (
      entries.length >= maxFrames
      || queuedBytes > maxBytes - bytes.byteLength
      || queuedDurationMs > maxDurationMs - durationMs
    ) {
      const removed = entries.shift();
      queuedBytes -= removed.size;
      queuedDurationMs -= removed.entry.durationMs;
      overflow++;
    }
    while (identities.size >= maxDedupEntries) {
      const oldestKey = identities.keys().next().value;
      identities.delete(oldestKey);
    }

    const ownedBytes = new Uint8Array(bytes.byteLength);
    ownedBytes.set(bytes);
    const entry = {key, receivedAtMs, durationMs, bytes: ownedBytes};
    entries.push({entry, size: bytes.byteLength});
    identities.set(key, {receivedAtMs});
    queuedBytes += bytes.byteLength;
    queuedDurationMs += durationMs;
    return {accepted: true};
  }

  function drain() {
    expireAt(readNow());
    const drained = entries.splice(0, entries.length).map(queued => queued.entry);
    queuedBytes = 0;
    queuedDurationMs = 0;
    return drained;
  }

  function clear() {
    entries.length = 0;
    identities.clear();
    queuedBytes = 0;
    queuedDurationMs = 0;
  }

  function snapshot() {
    expireAt(readNow());
    return {
      queuedBytes,
      queuedFrames: entries.length,
      queuedDurationMs,
      expired,
      overflow,
      duplicates,
    };
  }

  return {push, drain, clear, snapshot};
}
