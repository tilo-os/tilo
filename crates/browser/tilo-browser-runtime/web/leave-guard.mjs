/**
 * Closing or reloading the tab asks first only while leaving would lose or
 * hold back something the person did: a file still going up (any job the
 * desk runs), an edit this browser has not yet saved, or one the
 * workspace's host has not yet received. Otherwise the tab closes at once:
 * the browser's "Leave site?" prompt is never shown for nothing, and a page
 * with no `beforeunload` listener stays eligible for the back/forward cache.
 */

/** Why leaving now would lose work, or `null`: `jobs` running on the desk
 * (their titles), `pendingWrites` this browser has not saved, and
 * `outgoingPending` changes the host has not acknowledged. */
export function unfinishedWork({jobs = [], pendingWrites = 0, outgoingPending = 0} = {}) {
  if (jobs.length) return jobs[0];
  if (pendingWrites > 0) return 'Saving your changes';
  if (outgoingPending > 0) return 'Sending your changes to the workspace';
  return null;
}

/** Guard `window` against closing while `unfinishedWork` says so: `update`
 * with what is known now (any of its fields; the others keep their last
 * value). The listener stands only while there is work. */
export function createLeaveGuard({window: win = globalThis} = {}) {
  let state = {jobs: [], pendingWrites: 0, outgoingPending: 0};
  let listening = false;
  const ask = event => {
    if (!unfinishedWork(state)) return;
    event.preventDefault();
    // Older browsers ask only when a value is returned; none shows it.
    event.returnValue = '';
  };
  return {
    update(next) {
      state = {...state, ...Object.fromEntries(Object.entries(next ?? {}).filter(([, value]) => value !== undefined))};
      const busy = Boolean(unfinishedWork(state));
      if (busy && !listening) win.addEventListener?.('beforeunload', ask);
      if (!busy && listening) win.removeEventListener?.('beforeunload', ask);
      listening = busy;
    },
    get guarding() { return listening; },
  };
}
