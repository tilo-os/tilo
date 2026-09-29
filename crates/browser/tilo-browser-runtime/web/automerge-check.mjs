/**
 * Page side of the Automerge check for frames forwarded by other browsers
 * (see `automerge-check-worker.mjs` for what the check bounds). A check
 * resolves true only when the worker accepted the bytes within the deadline.
 * A timeout, refusal, worker error or undeliverable message resolves false,
 * terminates the worker and fails every check still in flight; the next
 * check starts a fresh worker. Without Worker support every check fails, so
 * forwarded frames are dropped and the durable delivery path carries them.
 */

/** Wall-clock deadline for one check, as for the native worker processes. */
export const AUTOMERGE_CHECK_TIMEOUT_MS = 5000;

function defaultCreateWorker() {
  if (typeof Worker !== 'function') return null;
  return new Worker(new URL('./automerge-check-worker.mjs', import.meta.url), {type: 'module'});
}

/** Transfer a view's buffer only when the view owns all of it. */
const owned = view => view instanceof Uint8Array && view.byteOffset === 0 && view.byteLength === view.buffer.byteLength;

export function createAutomergeGate({createWorker = defaultCreateWorker, timeoutMs = AUTOMERGE_CHECK_TIMEOUT_MS, clock = {}} = {}) {
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (handle => globalThis.clearTimeout(handle));
  const pending = new Map(); // id -> {resolve, timer}
  const stats = {checks: 0, accepted: 0, refused: 0, timeouts: 0, failures: 0, workers: 0};
  let worker = null;
  let next = 0;
  let closed = false;

  /** Terminate the current worker and fail every check it still owes. */
  function discard() {
    const current = worker;
    worker = null;
    if (current) { try { current.terminate(); } catch {} }
    for (const request of pending.values()) { clearTimer(request.timer); request.resolve(false); }
    pending.clear();
  }
  function spawn() {
    const created = createWorker();
    if (!created) return null;
    stats.workers++;
    created.addEventListener('message', event => {
      if (created !== worker) return;
      const {id, ok} = event.data ?? {};
      const request = pending.get(id);
      if (!request) return;
      pending.delete(id);
      clearTimer(request.timer);
      if (ok === true) { stats.accepted++; request.resolve(true); return; }
      // A refusal may have trapped the instance; never reuse it.
      stats.refused++;
      request.resolve(false);
      discard();
    });
    const failed = () => { if (created === worker) { stats.failures++; discard(); } };
    created.addEventListener('error', failed);
    created.addEventListener('messageerror', failed);
    return created;
  }

  return {
    /** Whether `incoming` applies cleanly to the document saved as `base`. */
    check(base, incoming) {
      if (closed) return Promise.resolve(false);
      stats.checks++;
      if (!worker) {
        try { worker = spawn(); } catch { worker = null; }
        if (!worker) { stats.failures++; return Promise.resolve(false); }
      }
      const id = ++next;
      const current = worker;
      return new Promise(resolve => {
        const timer = setTimer(() => {
          if (!pending.delete(id)) return;
          stats.timeouts++;
          resolve(false);
          if (worker === current) discard();
        }, timeoutMs);
        pending.set(id, {resolve, timer});
        const transfer = [base, incoming].filter(owned).map(view => view.buffer);
        try { current.postMessage({id, base, incoming}, [...new Set(transfer)]); }
        catch { stats.failures++; discard(); }
      });
    },
    close() { closed = true; discard(); },
    stats: () => ({...stats}),
  };
}
