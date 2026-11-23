// Something the page shows changed: a mount published a status (a frame,
// a window's state, the search), or the session published one (the
// workspace's state, its writes, who is here).
//
// A caller that waits for the page to reach some state (a test, a tool, an
// agent driving the page) reads that state again on each change instead of on
// a clock: `globalThis.tiloChanges.next(withinMs)` resolves `true` on the
// next change, `false` if none came within the time. `listen(fn)` calls `fn`
// on every change and answers the function that stops it.

const listeners = new Set();

/** Tell every listener that something the page shows changed. A listener
 * that throws is reported and the others are still told. */
export function changed() {
  for (const listener of [...listeners]) {
    try { listener(); } catch (error) { console.error('a page change listener failed', error); }
  }
}

/** Call `listener` on every change; answers the function that stops it. */
export function listen(listener) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

/** Resolves `true` on the next change, `false` when none came within
 * `withinMs`. */
export function next(withinMs) {
  return new Promise(resolve => {
    let timer;
    const stop = listen(() => { clearTimeout(timer); stop(); resolve(true); });
    timer = setTimeout(() => { stop(); resolve(false); }, Math.max(0, Number(withinMs) || 0));
  });
}

globalThis.tiloChanges ??= {listen, next};
