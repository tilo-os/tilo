// Web Worker that checks Automerge bytes forwarded by other browsers before
// the page admits them (`automerge-check.mjs` drives it).
//
// Automerge is not hardened against hostile input: malformed bytes can make
// it panic, loop or allocate without bound. On native hosts such bytes are
// decoded in a separate process with a heap cap and a deadline. A browser has
// no processes, so this worker runs its own instance of the same runtime
// module and applies the bytes to a copy of their base document there. Only
// a clean result lets the page make the identical call in its own instance.
//
// What that bounds:
// - CPU: a wall-clock deadline per check. The page terminates this worker
//   (`worker.terminate()`) when a check overruns, fails or refuses, and
//   starts a fresh one for the next frame.
// - Memory: only the wasm32 linear-memory ceiling of this worker's instance
//   (at most 4 GiB). The worker shares the page's renderer process, so a
//   hostile input can still pressure that process's memory before the
//   instance traps. This is much weaker than the native heap cap.
// - Scope: the page checks each frame against the document it applies to at
//   that moment. Frames that were already checked are later re-applied
//   without a new check when unconfirmed changes are rebased onto newer
//   durable state.
import init, {check_automerge} from './pkg/tilo_browser_runtime.js';

const ready = init();

self.addEventListener('message', async event => {
  const {id, base, incoming} = event.data ?? {};
  try {
    await ready;
    check_automerge(base, incoming);
    self.postMessage({id, ok: true});
  } catch (error) {
    self.postMessage({id, ok: false, error: String(error?.message ?? error)});
  }
});
