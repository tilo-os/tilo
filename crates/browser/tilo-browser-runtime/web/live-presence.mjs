/**
 * LIVE_V1 presence: publisher-side coalescing and receiver-side ordering,
 * interpolation and snapping. Pure logic; the transport and the canonical
 * codec (Rust/WASM) are injected.
 *
 * Presence values (the JSON projection of the frozen `PresenceFrame`):
 *   {focus:{app,record}|null, pointer:[x,y]|null,
 *    selection:{anchor:hex, head:hex}|null,
 *    drag:{object:{record:{app,record}|null,id:hex}, phase:'start'|'move'|'end'|'cancel',
 *          position:[x,y], size:[w,h]|null}|null}
 */

/** Relay ceiling is 120 presence frames/s per publisher (display rate up to 120 Hz). */
export const PRESENCE_MIN_INTERVAL_MS = 1000 / 120;
/**
 * Loss repair after the last change (datagrams are unreliable). The first
 * repeat also settles a stopped pointer quickly: receivers extrapolate the
 * last motion until the next frame arrives.
 */
export const PRESENCE_REPEATS_MS = Object.freeze([20, 200]);
/** Slow keepalive so late joiners see idle members; far below the rate ceiling. */
export const PRESENCE_KEEPALIVE_MS = 3000;
/**
 * Remote interpolation delay bounds. The delay adapts to the measured
 * arrival jitter (JITTER_FACTOR x the smoothed lateness of each frame
 * behind the fastest one observed), so a clean LAN renders ~4 ms behind the
 * newest frame and a jittery link buffers up to 34 ms.
 */
export const INTERPOLATION_MIN_MS = 4;
export const INTERPOLATION_MAX_MS = 34;
export const JITTER_FACTOR = 1.5;
/** Maximum extrapolation beyond the newest sample on loss. */
export const EXTRAPOLATION_MAX_MS = 80;
/**
 * Extrapolation beyond the newest sample, in publish intervals. With a
 * jitter-sized delay the renderer runs ahead of the newest frame for up to
 * one interval between arrivals; two intervals also bridge one lost frame
 * while bounding the overshoot when the remote pointer stops.
 */
export const EXTRAPOLATION_INTERVALS = 2;
/** An ended drag stays shown until the durable change lands or this timeout. */
export const ENDED_DRAG_TIMEOUT_MS = 5000;
/** A publisher with no frame for this long is reported stale. */
export const STALE_AFTER_MS = 10000;

const FIELDS = ['focus', 'pointer', 'selection', 'drag', 'participant'];
const PARTICIPANT_KINDS = new Set(['person', 'agent']);
const PHASES = new Set(['start', 'move', 'end', 'cancel']);

const emptyState = () => ({focus: null, pointer: null, selection: null, drag: null, participant: null});

function finitePair(value, name) {
  if (value === null) return null;
  if (!Array.isArray(value) || value.length !== 2 || !value.every(Number.isFinite)) {
    throw new TypeError(`${name} must be a finite [x, y] pair or null`);
  }
  return [value[0], value[1]];
}
function normalizeField(key, value) {
  if (value === undefined || value === null) return null;
  switch (key) {
    case 'pointer': return finitePair(value, 'pointer');
    case 'focus': return {app: String(value.app), record: String(value.record)};
    case 'selection': return {anchor: String(value.anchor), head: String(value.head)};
    case 'participant': {
      if (!PARTICIPANT_KINDS.has(value.kind)) throw new TypeError('participant kind must be person or agent');
      if (!/^#[0-9a-f]{6}$/.test(value.color)) throw new TypeError('participant colour must be #rrggbb');
      return {kind: value.kind, label: String(value.label), color: value.color, activity: value.activity == null ? null : String(value.activity)};
    }
    case 'drag': {
      if (!PHASES.has(value.phase)) throw new TypeError('drag phase must be start, move, end or cancel');
      return {
        object: {record: value.object?.record ? {app: String(value.object.record.app), record: String(value.object.record.record)} : null, id: String(value.object?.id)},
        phase: value.phase,
        position: finitePair(value.position, 'drag position'),
        size: value.size == null ? null : finitePair(value.size, 'drag size'),
      };
    }
    default: throw new TypeError(`unknown presence field ${key}`);
  }
}
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/**
 * Publisher: `update(partial)` merges fields. A change leaves at the end of
 * the current task (updates made together coalesce into one frame) unless a
 * frame left less than PRESENCE_MIN_INTERVAL_MS ago; then it leaves when the
 * interval elapses, carrying the latest value. Input events arrive at display
 * rate, so the publish rate follows the display up to 120 Hz without waiting
 * for an animation frame. After the last change: loss-repair repeats, then a
 * slow keepalive. A drag that ended is cleared after its repeats.
 */
export function createPresencePublisher({
  encode,
  send,
  now = () => performance.now(),
  wallNow = () => Date.now(),
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
  /** End-of-task scheduling (coalesces updates made in one event handler). */
  defer = callback => globalThis.queueMicrotask(callback),
  onPublish = () => {},
  keepaliveMs = PRESENCE_KEEPALIVE_MS,
  // The relay forwards only strictly increasing sequence numbers per
  // publisher; seeding from the wall clock (microseconds, < 2^53) keeps a
  // reloaded page ahead of its previous incarnation.
  initialSeq = Math.floor(wallNow()) * 1000,
}) {
  if (typeof encode !== 'function' || typeof send !== 'function') throw new TypeError('encode and send are required');
  let state = emptyState();
  if (!Number.isSafeInteger(initialSeq) || initialSeq < 0) throw new TypeError('initialSeq must be a safe non-negative integer');
  let seq = initialSeq;
  let dirty = false;
  let pending; // undefined | 'task' | timer handle of a rate-limited send
  let pendingGeneration = 0;
  let timer;
  let lastSentAt = -Infinity;
  let active = false;
  let stopped = false;
  let repeatIndex = 0;
  const stats = {published: 0, repeats: 0, coalesced: 0, dropped: 0};

  const hasContent = () => FIELDS.some(key => state[key] !== null);
  const clearTimers = () => {
    if (pending !== undefined && pending !== 'task') clearTimer(pending);
    pending = undefined;
    pendingGeneration++;
    if (timer !== undefined) clearTimer(timer);
    timer = undefined;
  };
  function transmit(repeat) {
    seq += 1;
    const value = {seq, sentAtMs: Math.round(wallNow()), ...state};
    let bytes;
    try { bytes = encode(value); } catch (error) { stats.dropped++; throw error; }
    const publishedAtMs = now();
    const sent = send(bytes, value) !== false;
    if (!sent) { stats.dropped++; return false; }
    lastSentAt = publishedAtMs;
    if (repeat) stats.repeats++; else stats.published++;
    try { onPublish({kind: 'presence', seq, sentAtMs: value.sentAtMs, publishedAtMs, bytes: bytes.byteLength, repeat, frame: value}); } catch {}
    return true;
  }
  function armRepeat() {
    if (timer !== undefined) clearTimer(timer);
    timer = undefined;
    if (!active || stopped) return;
    let delay;
    if (repeatIndex < PRESENCE_REPEATS_MS.length) {
      delay = PRESENCE_REPEATS_MS[repeatIndex] - (repeatIndex ? PRESENCE_REPEATS_MS[repeatIndex - 1] : 0);
    } else if (hasContent() && keepaliveMs > 0) {
      delay = keepaliveMs;
    } else return;
    timer = setTimer(() => {
      timer = undefined;
      if (!active || stopped || dirty) return;
      const final = repeatIndex === PRESENCE_REPEATS_MS.length - 1;
      repeatIndex++;
      transmit(true);
      // An ended or cancelled drag is published (and repeated) once, then cleared.
      if (final && state.drag && (state.drag.phase === 'end' || state.drag.phase === 'cancel')) {
        state = {...state, drag: null};
        dirty = true;
        schedule();
        return;
      }
      armRepeat();
    }, delay);
  }
  // Time until the rate limit admits the next frame (strictly <= 120/s for
  // the relay; <= 0.05 ms counts as now, so timer rounding cannot spin on a
  // sub-microsecond remainder).
  const waitMs = () => {
    const wait = PRESENCE_MIN_INTERVAL_MS - (now() - lastSentAt);
    return wait > 0.05 ? wait : 0;
  };
  function tick() {
    pending = undefined;
    if (!dirty || !active || stopped) return;
    if (waitMs() > 0) { schedule(); return; }
    dirty = false;
    repeatIndex = 0;
    transmit(false);
    armRepeat();
  }
  function schedule() {
    if (!active || stopped || pending !== undefined) return;
    const wait = waitMs();
    if (wait > 0) {
      pending = setTimer(tick, wait);
      return;
    }
    pending = 'task';
    const generation = pendingGeneration;
    defer(() => { if (generation === pendingGeneration && pending === 'task') tick(); });
  }
  return {
    /** Merge fields; `undefined` keeps a field, `null` clears it. */
    update(partial) {
      if (stopped) return false;
      const next = {...state};
      for (const key of Object.keys(partial ?? {})) {
        if (!FIELDS.includes(key)) throw new TypeError(`unknown presence field ${key}`);
        if (partial[key] === undefined) continue;
        next[key] = normalizeField(key, partial[key]);
      }
      if (same(next, state)) return false;
      if (dirty) stats.coalesced++;
      state = next;
      dirty = true;
      schedule();
      return true;
    },
    /** Replace the whole state. */
    replace(value) {
      return this.update({...emptyState(), ...value});
    },
    state: () => structuredClone(state),
    /** Transport available: publish the current state if it has content. */
    start() {
      if (stopped) return;
      active = true;
      if (hasContent()) { dirty = true; schedule(); }
    },
    /** Transport lost: keep the state, stop sending. */
    pause() {
      active = false;
      clearTimers();
    },
    stop() {
      stopped = true;
      active = false;
      clearTimers();
    },
    stats: () => ({...stats, seq}),
  };
}

function lerp(a, b, t) {
  return a + (b - a) * t;
}
function lerpPair(a, b, t) {
  return [lerp(a[0], b[0], t), lerp(a[1], b[1], t)];
}

/**
 * Receiver for one remote publisher. Frames with a sequence at or below the
 * newest accepted one are dropped. `sample(now)` renders the state a
 * jitter-sized delay in the past (interpolated between frames), extrapolates
 * the last motion for up to two publish intervals past the newest frame
 * (between arrivals and on loss), and snaps on drag End/Cancel or after
 * `snap()`.
 *
 * Timing model: the publisher's send clock is mapped onto ours by the
 * minimum observed (arrival - send). Each frame's lateness behind that
 * minimum is its network/scheduling jitter; the delay is JITTER_FACTOR x the
 * smoothed lateness (fast rise, slow decay), clamped to
 * [INTERPOLATION_MIN_MS, INTERPOLATION_MAX_MS].
 */
export function createRemotePresence({maxSamples = 8} = {}) {
  const samples = [];
  let newest;
  let lastSeq = -1;
  let offset = Infinity;
  let interval = 16;
  let jitter = 0;
  const ended = new Map();
  const stats = {accepted: 0, stale: 0, duplicate: 0};

  function accept(frame, receivedAtMs) {
    if (!Number.isSafeInteger(frame?.seq)) { stats.stale++; return false; }
    if (frame.seq <= lastSeq) {
      // A publisher that restarted without a newer seed: accept once its
      // send time is clearly newer, and restart the history.
      const restarted = newest && frame.seq < lastSeq && frame.sentAtMs > newest.sentAtMs + 1000;
      if (!restarted) {
        stats[frame.seq === lastSeq ? 'duplicate' : 'stale']++;
        return false;
      }
      samples.length = 0;
      offset = Infinity;
      jitter = 0;
    }
    lastSeq = frame.seq;
    const sample = {
      seq: frame.seq,
      sentAtMs: frame.sentAtMs,
      receivedAtMs,
      focus: frame.focus ?? null,
      pointer: frame.pointer ?? null,
      selection: frame.selection ?? null,
      drag: frame.drag ?? null,
      participant: frame.participant ?? null,
    };
    // Coordinates belong to the focused surface. Never interpolate a point
    // from the previous app/record under the newest frame's focus.
    if (!same(newest?.focus ?? null, sample.focus)) samples.length = 0;
    const previous = samples.at(-1);
    if (previous && sample.sentAtMs > previous.sentAtMs) {
      const gap = sample.sentAtMs - previous.sentAtMs;
      // Repeats and keepalives are not motion cadence.
      if (gap < 100) interval = interval * 0.8 + gap * 0.2;
    }
    // Minimum observed (arrival - send) maps the publisher's clock onto ours.
    offset = Math.min(offset, receivedAtMs - sample.sentAtMs);
    samples.push(sample);
    while (samples.length > maxSamples) samples.shift();
    if (samples.length === maxSamples) {
      // Re-estimate over the window so clock drift cannot pin a stale minimum.
      offset = Math.min(...samples.map(s => s.receivedAtMs - s.sentAtMs));
    }
    // Lateness behind the fastest arrival: fast rise, slow decay, so one
    // late burst widens the buffer at once and a clean link narrows it again
    // over ~1 s of motion.
    const late = Math.max(0, receivedAtMs - sample.sentAtMs - offset);
    jitter += (late - jitter) * (late > jitter ? 0.3 : 0.02);
    newest = sample;
    stats.accepted++;
    const drag = sample.drag;
    if (drag && (drag.phase === 'end' || drag.phase === 'cancel')) {
      ended.set(drag.object.id, {drag, at: receivedAtMs});
    } else if (drag) {
      ended.delete(drag.object.id);
    }
    return true;
  }
  const local = sample => sample.sentAtMs + offset;
  const delay = () => Math.min(INTERPOLATION_MAX_MS, Math.max(INTERPOLATION_MIN_MS, JITTER_FACTOR * jitter));

  function position(pick, at, compatible) {
    const valued = samples.filter(s => pick(s) !== null);
    if (valued.length === 0) return null;
    const last = valued.at(-1);
    // The newest frame cleared the value: honour it immediately.
    if (newest && pick(newest) === null) return null;
    if (valued.length === 1 || at <= local(valued[0])) return pick(at <= local(valued[0]) ? valued[0] : last);
    for (let i = valued.length - 1; i > 0; i--) {
      const a = valued[i - 1];
      const b = valued[i];
      if (local(a) <= at && at <= local(b)) {
        if (!compatible(a, b)) return pick(at - local(a) < local(b) - at ? a : b);
        const span = local(b) - local(a);
        return lerpPair(pick(a), pick(b), span > 0 ? (at - local(a)) / span : 1);
      }
    }
    // Past the newest sample: extrapolate from the last motion, bounded.
    const before = valued.at(-2);
    const ahead = Math.min(at - local(last), EXTRAPOLATION_MAX_MS, EXTRAPOLATION_INTERVALS * interval);
    const span = local(last) - local(before);
    if (!compatible(before, last) || span <= 0 || span > 100) return pick(last);
    const velocity = [(pick(last)[0] - pick(before)[0]) / span, (pick(last)[1] - pick(before)[1]) / span];
    return [pick(last)[0] + velocity[0] * ahead, pick(last)[1] + velocity[1] * ahead];
  }

  function sample(nowMs) {
    if (!newest) return null;
    const at = nowMs - delay();
    const pointer = position(s => s.pointer, at, () => true);
    let drag = null;
    const current = newest.drag;
    if (current && (current.phase === 'end' || current.phase === 'cancel')) {
      drag = {...current, snapped: true};
    } else if (current) {
      const sameObject = (a, b) => a.drag && b.drag && a.drag.object.id === b.drag.object.id
        && a.drag.phase !== 'end' && a.drag.phase !== 'cancel' && b.drag.phase !== 'end' && b.drag.phase !== 'cancel';
      const pos = position(s => (s.drag && s.drag.object.id === current.object.id ? s.drag.position : null), at, sameObject);
      const sizeValue = current.size
        ? position(s => (s.drag && s.drag.object.id === current.object.id ? s.drag.size : null), at, sameObject)
        : null;
      drag = {...current, position: pos ?? current.position, size: sizeValue ?? current.size, snapped: false};
    }
    const endedDrags = [];
    for (const [id, entry] of ended) {
      if (nowMs - entry.at > ENDED_DRAG_TIMEOUT_MS) ended.delete(id);
      else if (!drag || drag.object.id !== id) endedDrags.push({...entry.drag, snapped: true});
    }
    return {
      seq: newest.seq,
      sentAtMs: newest.sentAtMs,
      receivedAtMs: newest.receivedAtMs,
      focus: newest.focus,
      selection: newest.selection,
      /** `{kind: 'person' | 'agent', label, color, activity}` or `null`. */
      participant: newest.participant,
      pointer,
      drag,
      endedDrags,
      stale: nowMs - newest.receivedAtMs > STALE_AFTER_MS,
    };
  }

  return {
    accept,
    sample,
    /** Durable change landed (or a local reset): drop history, keep newest raw. */
    snap(objectId) {
      if (objectId === undefined) {
        ended.clear();
        samples.splice(0, samples.length - 1);
      } else {
        ended.delete(objectId);
        if (newest?.drag?.object.id === objectId) samples.splice(0, samples.length - 1);
      }
    },
    lastSeq: () => lastSeq,
    newest: () => newest,
    interpolationDelayMs: delay,
    jitterMs: () => jitter,
    intervalMs: () => interval,
    stats: () => ({...stats}),
  };
}
