/**
 * Lifecycle guard, exponential backoff and circuit breaker for browser
 * `WebTransport` instances.
 *
 * On macOS WebKit (`com.apple.WebKit.Networking`), each `new WebTransport(...)`
 * allocates a QUIC UDP socket that is not released promptly unless `close()` is
 * called when the handshake fails, times out, is aborted, or the session ends.
 * A reconnect loop or refused device can exhaust the per-process file
 * descriptor limit and freeze every QUIC connection on the machine.
 *
 * Installing this guard on `globalThis` wraps `WebTransport` transparently (it
 * still returns the native `WebTransport` instance so wasm callers keep full
 * WebIDL compatibility):
 * - Every constructed transport is tracked and closed idempotently on `ready`
 *   rejection, handshake timeout, `closed` settlement, page teardown, or
 *   explicit origin teardown.
 * - Repeated failures or short-lived server drops apply exponential backoff
 *   and open a circuit breaker so a broken network or revoked session cannot
 *   hammer `new WebTransport(...)` in a tight loop.
 * - A permanently halted origin (such as a revoked device) rejects before
 *   constructing any native `WebTransport`, allocating zero UDP sockets.
 */
import {backoffDelay} from './reconnect.mjs';

/** Exponential backoff shape after consecutive transport failures. */
export const TRANSPORT_BACKOFF = Object.freeze({firstMs: 1000, factor: 2, capMs: 60_000, jitter: 0.2});
/** Consecutive failures before the circuit breaker opens for an origin. */
export const BREAKER_THRESHOLD = 5;
/** Initial cooldown while the circuit breaker is open. */
export const BREAKER_COOLDOWN_MS = 30_000;
/** Longest cooldown while the circuit breaker keeps tripping. */
export const BREAKER_MAX_COOLDOWN_MS = 300_000;
/** Maximum time a WebTransport handshake may stay pending before closing. */
export const HANDSHAKE_TIMEOUT_MS = 20_000;
/** A transport closed by the server sooner than this counts as a failure. */
export const SHORT_LIVED_MS = 2_000;
/** Hard ceiling on simultaneous native WebTransport objects in this page. */
export const MAX_ACTIVE_TRANSPORTS = 32;

const GUARD_SYMBOL = Symbol.for('tilo.webtransport.guard');

/** Normalize `url` to its origin key (`https://example:443`), or raw string. */
export function transportOrigin(url) {
  try {
    return new URL(String(url)).origin;
  } catch {
    return String(url ?? '');
  }
}

/** Lightweight rejected transport stub returned without allocating a socket. */
function rejectedTransport(code, message) {
  const error = Object.assign(new Error(message), {name: 'WebTransportError', code});
  const ready = Promise.reject(error);
  const closed = Promise.reject(error);
  // Prevent unhandled rejection noise when callers only await `ready`.
  ready.catch(() => {});
  closed.catch(() => {});
  return {
    ready,
    closed,
    protocol: '',
    maxDatagramSize: 0,
    close() {},
  };
}

/** No-op guard when the environment has no `WebTransport` constructor. */
const NOOP_GUARD = Object.freeze({
  activeCount: () => 0,
  closeOrigin: () => 0,
  haltOrigin: () => 0,
  allowOrigin: () => {},
  resetOrigin: () => {},
  closeAll: () => 0,
  stats: () => ({active: 0, failures: 0, halted: false, openUntil: 0, nextAllowedAt: 0, trips: 0}),
  uninstall: () => {},
});

/**
 * Wrap `scope.WebTransport` once with lifecycle teardown, backoff, and a
 * per-origin circuit breaker. Returns the guard controller.
 */
export function installWebTransportGuard({
  scope = globalThis,
  now = () => Date.now(),
  setTimer = (callback, ms) => globalThis.setTimeout(callback, ms),
  clearTimer = handle => globalThis.clearTimeout(handle),
  random = Math.random,
  backoff = TRANSPORT_BACKOFF,
  breakerThreshold = BREAKER_THRESHOLD,
  breakerCooldownMs = BREAKER_COOLDOWN_MS,
  breakerMaxCooldownMs = BREAKER_MAX_COOLDOWN_MS,
  handshakeTimeoutMs = HANDSHAKE_TIMEOUT_MS,
  shortLivedMs = SHORT_LIVED_MS,
  maxActive = MAX_ACTIVE_TRANSPORTS,
} = {}) {
  if (!scope || typeof scope.WebTransport !== 'function') return NOOP_GUARD;
  if (scope[GUARD_SYMBOL]) return scope[GUARD_SYMBOL];

  const NativeWebTransport = scope.WebTransport;
  const nativeClose = NativeWebTransport.prototype?.close;
  const active = new Set();
  const origins = new Map();

  const originState = key => {
    let state = origins.get(key);
    if (!state) {
      state = {failures: 0, nextAllowedAt: 0, openUntil: 0, trips: 0, halted: false};
      origins.set(key, state);
    }
    return state;
  };

  const noteFailure = key => {
    const state = originState(key);
    const current = now();
    state.failures += 1;
    const delay = backoffDelay(state.failures - 1, random, backoff);
    state.nextAllowedAt = Math.max(state.nextAllowedAt, current + delay);
    if (state.failures >= breakerThreshold) {
      state.trips += 1;
      const cooldown = Math.min(breakerMaxCooldownMs, breakerCooldownMs * (2 ** Math.min(state.trips - 1, 4)));
      state.openUntil = Math.max(state.openUntil, current + cooldown);
    }
  };

  const noteSuccess = key => {
    const state = origins.get(key);
    if (!state || state.halted) return;
    state.failures = 0;
    state.nextAllowedAt = 0;
    state.openUntil = 0;
    state.trips = 0;
  };

  const closeOrigin = origin => {
    const key = transportOrigin(origin);
    let closedCount = 0;
    for (const entry of [...active]) {
      if (entry.origin === key) {
        entry.close({closeCode: 0, reason: 'closed'});
        closedCount += 1;
      }
    }
    return closedCount;
  };

  const closeAll = () => {
    let closedCount = 0;
    for (const entry of [...active]) {
      entry.close({closeCode: 0, reason: 'page closing'});
      closedCount += 1;
    }
    return closedCount;
  };

  const haltOrigin = origin => {
    const key = transportOrigin(origin);
    const state = originState(key);
    state.halted = true;
    return closeOrigin(key);
  };

  const allowOrigin = origin => {
    const key = transportOrigin(origin);
    const state = origins.get(key);
    if (!state) return;
    state.halted = false;
    state.failures = 0;
    state.nextAllowedAt = 0;
    state.openUntil = 0;
    state.trips = 0;
  };

  const resetOrigin = origin => {
    const key = transportOrigin(origin);
    const state = origins.get(key);
    if (!state || state.halted) return;
    state.failures = 0;
    state.nextAllowedAt = 0;
    state.openUntil = 0;
    state.trips = 0;
  };

  /** Sweep unready stale transports if the active ceiling is hit. */
  const reclaimIfFull = current => {
    if (active.size < maxActive) return;
    for (const entry of [...active]) {
      if (!entry.opened && current - entry.createdAt >= handshakeTimeoutMs) {
        entry.close({closeCode: 0, reason: 'stale handshake'});
      }
    }
  };

  function GuardedWebTransport(url, options) {
    const key = transportOrigin(url);
    const state = originState(key);
    const current = now();
    if (state.halted) {
      return rejectedTransport('device-revoked', `WebTransport halted for ${key}: device was revoked`);
    }
    if (current < state.openUntil) {
      return rejectedTransport('transport-circuit-open', `WebTransport circuit open for ${key}`);
    }
    if (current < state.nextAllowedAt) {
      return rejectedTransport('transport-backoff', `WebTransport backoff active for ${key}`);
    }
    reclaimIfFull(current);
    if (active.size >= maxActive) {
      return rejectedTransport('transport-limit', `WebTransport active socket limit (${maxActive}) reached`);
    }

    let transport;
    try {
      transport = options === undefined
        ? new NativeWebTransport(url)
        : new NativeWebTransport(url, options);
    } catch (error) {
      noteFailure(key);
      throw error;
    }

    let closed = false;
    let closedByClient = false;
    let opened = false;
    let openedAt = 0;
    let handshakeTimer = null;
    let stabilityTimer = null;
    let failureCounted = false;

    const markFailureOnce = () => {
      if (failureCounted) return;
      failureCounted = true;
      noteFailure(key);
    };

    const rawClose = typeof transport.close === 'function'
      ? transport.close.bind(transport)
      : (typeof nativeClose === 'function' ? nativeClose.bind(transport) : null);

    const closeOnce = info => {
      if (closed) return;
      closed = true;
      if (handshakeTimer !== null) { clearTimer(handshakeTimer); handshakeTimer = null; }
      if (stabilityTimer !== null) { clearTimer(stabilityTimer); stabilityTimer = null; }
      active.delete(entry);
      if (!rawClose) return;
      try {
        if (info !== undefined) rawClose(info);
        else rawClose();
      } catch {
        // Closing an already-closed WebTransport is safe to ignore.
      }
    };

    const entry = {
      origin: key,
      createdAt: current,
      get opened() { return opened; },
      close(info) {
        closedByClient = true;
        closeOnce(info);
      },
    };
    active.add(entry);

    // Intercept caller-initiated close() so we clear timers and distinguish
    // intentional client teardown from server-side drops.
    try {
      Object.defineProperty(transport, 'close', {
        configurable: true,
        writable: true,
        value(info) {
          closedByClient = true;
          closeOnce(info);
        },
      });
    } catch {
      // Non-configurable transport objects still close cleanly via `entry.close`.
    }

    if (handshakeTimeoutMs > 0) {
      handshakeTimer = setTimer(() => {
        handshakeTimer = null;
        if (!opened && !closed) {
          markFailureOnce();
          closeOnce({closeCode: 0, reason: 'handshake timeout'});
        }
      }, handshakeTimeoutMs);
    }

    if (transport.ready && typeof transport.ready.then === 'function') {
      transport.ready.then(
        () => {
          if (closed) return;
          opened = true;
          openedAt = now();
          if (handshakeTimer !== null) { clearTimer(handshakeTimer); handshakeTimer = null; }
          if (shortLivedMs > 0) {
            stabilityTimer = setTimer(() => {
              stabilityTimer = null;
              if (!closed) noteSuccess(key);
            }, shortLivedMs);
          } else {
            noteSuccess(key);
          }
        },
        () => {
          if (!closedByClient) markFailureOnce();
          closeOnce();
        },
      );
    }

    if (transport.closed && typeof transport.closed.then === 'function') {
      const onSettled = () => {
        if (!closedByClient && (!opened || (now() - openedAt) < shortLivedMs)) {
          markFailureOnce();
        } else if (opened && (now() - openedAt) >= shortLivedMs) {
          noteSuccess(key);
        }
        closeOnce();
      };
      transport.closed.then(onSettled, onSettled);
    }

    return transport;
  }

  GuardedWebTransport.prototype = NativeWebTransport.prototype;
  Object.setPrototypeOf(GuardedWebTransport, NativeWebTransport);

  const onPageHide = event => {
    if (event?.persisted) return;
    closeAll();
  };
  const onBeforeUnload = () => { closeAll(); };
  const onOnline = () => {
    for (const [key, state] of origins) {
      if (!state.halted) resetOrigin(key);
    }
  };
  scope.addEventListener?.('pagehide', onPageHide);
  scope.addEventListener?.('beforeunload', onBeforeUnload);
  scope.addEventListener?.('online', onOnline);

  const guard = {
    activeCount: origin => {
      if (origin === undefined) return active.size;
      const key = transportOrigin(origin);
      let count = 0;
      for (const entry of active) if (entry.origin === key) count += 1;
      return count;
    },
    closeOrigin,
    haltOrigin,
    allowOrigin,
    resetOrigin,
    closeAll,
    stats(origin) {
      if (origin === undefined) {
        return {
          active: active.size,
          origins: Object.fromEntries([...origins].map(([key, state]) => [key, {...state}])),
        };
      }
      const key = transportOrigin(origin);
      const state = origins.get(key) ?? {failures: 0, nextAllowedAt: 0, openUntil: 0, trips: 0, halted: false};
      return {active: guard.activeCount(key), ...state};
    },
    uninstall() {
      closeAll();
      scope.removeEventListener?.('pagehide', onPageHide);
      scope.removeEventListener?.('beforeunload', onBeforeUnload);
      scope.removeEventListener?.('online', onOnline);
      if (scope.WebTransport === GuardedWebTransport) scope.WebTransport = NativeWebTransport;
      delete scope[GUARD_SYMBOL];
    },
  };

  scope.WebTransport = GuardedWebTransport;
  Object.defineProperty(scope, GUARD_SYMBOL, {value: guard, configurable: true});
  return guard;
}

/** Return the installed WebTransport guard for `scope`, installing it on first use. */
export function webTransportGuard(scope = globalThis) {
  return scope?.[GUARD_SYMBOL] ?? installWebTransportGuard({scope});
}
