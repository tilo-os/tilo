/**
 * Getting in again after a host did not let the page in yet: the one
 * reconnect screen of the entry page, for every reason it waits.
 *
 * - `unreachable`: the host does not answer (restarting, or the network
 *   between);
 * - `refused`: the host turned this browser away at its entry, and the page
 *   comes again as a new device of the person (`workspaces.mjs`);
 * - `offline`: the browser itself is offline;
 * - `busy`: the host has no workspace for the page now.
 *
 * Between tries the page waits with exponential backoff and jitter
 * (`backoffDelay`), paused while the tab is hidden or the browser offline
 * and resumed at once when it is back; "Try now" tries at once. The screen
 * says what happened in plain words, counts down to the next try and keeps
 * the technical details (`reconnectDetails`, never a token, key or
 * certificate) folded below, to copy.
 *
 * A wait the page shows nothing of (`wait({impatient: true})`: the workspace
 * here is held by another tab of this browser, which the bar says) puts no
 * screen up: it resolves `true` when the page is to look again.
 */
import {localized} from './arrival.mjs';
import {deviceRemoved, permanentRefusal} from './session-refusal.mjs';

/** The first wait, the factor each next one grows by, the longest wait and
 * the share of a wait that is random either way. */
export const BACKOFF = {firstMs: 2000, factor: 2, capMs: 5 * 60 * 1000, jitter: 0.2};

/** How soon a page that shows nothing of its wait (another tab of this
 * browser holding the key the workspace here is opened with) looks again: a
 * tab closing is noticed within a second. */
export const WAIT_LOOK_MS = 1000;
/** How that page's looks grow: a tab left open all afternoon is not looked
 * at every second. */
const LOOK_BACKOFF = {firstMs: WAIT_LOOK_MS, factor: 1.5, capMs: 10_000, jitter: 0.2};

/** `start`, `factor`, `capMs` and `jitter` of a wait series, shaped like
 * [`BACKOFF`]. */
const shape = start => ({
  firstMs: start.firstMs, factor: start.factor, capMs: start.capMs, jitter: start.jitter ?? BACKOFF.jitter,
});

/** How long to wait before try `attempt` (0 for the first wait): exponential
 * from `start.firstMs` (by default [`BACKOFF`]), ±`jitter`, never past
 * `capMs`. The wait is random so a workspace full of pages that lost one host do
 * not all come back in the same tick. */
export function backoffDelay(attempt, random = Math.random, start = BACKOFF) {
  const {firstMs, factor, capMs, jitter} = shape(start);
  const steps = Math.max(0, Math.floor(attempt));
  const base = Math.min(capMs, firstMs * factor ** steps);
  const jittered = base * (1 + jitter * (2 * random() - 1));
  return Math.round(Math.min(capMs, jittered));
}

/** Why the page waits, from the last `error` and whether the browser is
 * `online`: `device-revoked`, `offline`, `refused`, `busy` or `unreachable`. */
export function reconnectReason(error, {online = true} = {}) {
  if (deviceRemoved(error)) return 'device-revoked';
  if (online === false) return 'offline';
  if (error?.code === 'refused') return 'refused';
  if (['full', 'too-many'].includes(error?.code) || error?.status === 429) return 'busy';
  return 'unreachable';
}

/** Long hex or base64url runs (keys, tokens, certificates, signatures) and
 * secret link parameters, as they could appear in an error's words. */
const SECRETS = [
  /([?&#](?:invite|token|ticket|pair|key|secret)=)[^&\s"']+/gi,
  /\b[0-9a-f]{32,}\b/gi,
  /[A-Za-z0-9_-]{32,}={0,2}/g,
];

/** `text` with anything that could be a secret replaced by `[redacted]`. */
export function redact(text) {
  let out = String(text ?? '');
  out = out.replace(SECRETS[0], '$1[redacted]');
  for (const pattern of SECRETS.slice(1)) out = out.replace(pattern, '[redacted]');
  return out;
}

/** An error in one line: its name and words, and the codes the host gave. */
export function describeError(error) {
  if (error == null) return 'none';
  const parts = [`${error.name ?? 'Error'}: ${error.message ?? String(error)}`];
  if (error.code) parts.push(`code ${error.code}`);
  if (error.status) parts.push(`HTTP ${error.status}`);
  if (error.retryAfter) parts.push(`retry after ${error.retryAfter} s`);
  if (error.reason && error.reason !== error.message) parts.push(`host said: ${error.reason}`);
  if (error.phase) parts.push(`session ${error.phase}${error.connected ? ', connected' : ''}`);
  if (error.sessionError) parts.push(`session said: ${error.sessionError}${error.sessionErrorCode ? ` (${error.sessionErrorCode})` : ''}`);
  return redact(parts.join(' · '));
}

/** The host's address as a screen names it: `host[:port]`, no scheme. */
export function hostName(origin) {
  try { return new URL(origin).host; } catch { return null; }
}

/** Whether `origin` is a machine's own address (an IP address, `localhost`
 * or a local network name) rather than a public name: only such a host can
 * serve a certificate this browser was never told to trust. */
export function localHost(origin) {
  let host;
  try { host = new URL(origin).hostname; } catch { return false; }
  return /^\[?[0-9a-f:.]+\]?$/i.test(host) && /[.:]/.test(host)
    || host === 'localhost' || /\.(localhost|local|lan|internal|home\.arpa)$/i.test(host);
}

/** The origin of `url` alone (no path, no query, no fragment), or `unknown`. */
function originOnly(url) {
  try { return new URL(url).origin; } catch { return 'unknown'; }
}

/**
 * The technical details of the wait, as text to read or copy: the last
 * error, how many tries failed since when, the host's origin and the page's
 * build. Never a token, key or certificate.
 */
export function reconnectDetails({reason, error, attempt, since, origin, build}) {
  return [
    `Reason: ${reason}`,
    `Last error: ${describeError(error)}`,
    `Failed tries: ${attempt + 1}`,
    `Failing since: ${since ? new Date(since).toISOString() : 'unknown'}`,
    `Host: ${originOnly(origin)}`,
    `Page build: ${redact(build ?? 'unknown')}`,
  ].join('\n');
}

/** `ms` as the countdown says it: seconds under a minute, else minutes and
 * seconds. */
export function countdownText(ms) {
  const seconds = Math.max(0, Math.ceil(ms / 1000));
  if (seconds < 60) return `${seconds} s`;
  const rest = seconds % 60;
  return rest ? `${Math.floor(seconds / 60)} min ${String(rest).padStart(2, '0')} s` : `${seconds / 60} min`;
}

/**
 * The reconnect wait, driving `view` (`{show(state), hide()}`). `wait(...)`
 * resolves once the page should try again: after the backoff, at once on
 * "Try now" (`tryNow()`), when the browser comes back online, or when the
 * tab is shown again after its time passed. While the page tries, the view
 * says so (`trying`); `done()` hides it once the page got in.
 */
export function createReconnect({view, window = globalThis, document = globalThis.document, now = () => Date.now(),
  schedule = (run, ms) => setTimeout(run, ms), cancel = clearTimeout, random = Math.random} = {}) {
  let current = null;
  const online = () => window?.navigator?.onLine !== false;
  const hidden = () => document?.visibilityState === 'hidden';
  const render = () => {
    // A page that shows nothing of the wait has its own line on the bar
    // (`connection-status.mjs`): the screen is not its to take.
    if (!current || current.quiet) return;
    const waiting = !online() ? 'offline' : current.trying ? 'trying' : 'counting';
    view.show({...current.info, reason: online() ? current.info.reason : 'offline', waiting,
      remainingMs: Math.max(0, current.deadline - now())});
  };
  const fire = () => {
    if (!current || current.trying) return;
    cancel(current.timer);
    current.trying = true;
    render();
    const {resolve} = current;
    resolve();
  };
  const tick = () => {
    if (!current || current.trying) return;
    cancel(current.timer);
    // Paused: nothing is tried while the tab is hidden or the browser offline.
    if (hidden() || !online()) { render(); return; }
    const remaining = current.deadline - now();
    if (remaining <= 0) { fire(); return; }
    render();
    // The next look at the clock: a whole second away while more than one is
    // left, else the moment it expires (`260` ms, not `2` ms short of it and
    // a whole second late).
    current.timer = schedule(tick, Math.min(1000, remaining));
  };
  const onOnline = () => { if (current && !current.trying) fire(); };
  const onOffline = () => tick();
  const onVisible = () => { tick(); };
  // A hidden tab keeps no timer of its own for long: coming back to it is a
  // moment the page is sure to be woken, so the clock is read again then.
  window?.addEventListener?.('online', onOnline);
  window?.addEventListener?.('offline', onOffline);
  document?.addEventListener?.('visibilitychange', onVisible);
  return {
    /** Wait before try `attempt` (0 for the first wait), the last one having
     * failed with `error`; `retryAfterMs` is the least wait the host asked
     * for. `impatient` is a page that shows nothing of the wait — another tab
     * of this browser holding the key the workspace here is opened with, say —
     * so it looks again on a clock of its own and resolves `true` when a look
     * is due, the person's "Try now" included. Rejects when `signal` aborts; a
     * wait another wait replaces, or one `done` ends, settles nothing: the
     * page went another way. */
    wait({reason, error, attempt = 0, since = now(), origin, build, retryAfterMs = 0, signal, impatient = false} = {}) {
      // A wait left unclaimed by a page that went another way (a reload, a
      // throw above it) never resolves: it must not outlive its own try, nor
      // cancel the wait that replaced it.
      const self = {};
      if (current) cancel(current.timer);
      if (permanentRefusal(error) || ['device-revoked', 'device_revoked', 'workspace-deleted'].includes(reason)) {
        current = null;
        view.hide();
        const code = deviceRemoved(error) || reason === 'device_revoked'
          ? 'device-revoked'
          : (error?.code ?? reason ?? 'refused');
        return Promise.reject(error instanceof Error
          ? (error.code ? error : Object.assign(error, {code}))
          : Object.assign(new Error(String(error?.message ?? 'Session permanently refused')), {code}));
      }
      return new Promise((resolve, reject) => {
        signal?.throwIfAborted();
        const delay = impatient
          ? Math.max(WAIT_LOOK_MS, Math.round(backoffDelay(attempt, random, LOOK_BACKOFF) / WAIT_LOOK_MS) * WAIT_LOOK_MS)
          : Math.max(backoffDelay(attempt, random), retryAfterMs ?? 0);
        current = {info: {reason: reason ?? reconnectReason(error, {online: online()}), error, attempt, since, origin, build},
          deadline: now() + delay, trying: false, resolve: () => resolve(impatient), timer: null, self, quiet: impatient};
        signal?.addEventListener('abort', () => {
          // Only the wait an abort hears now is the one it stops: an abort
          // heard after the next wait began would leave that one uncounted.
          if (current?.self !== self) return;
          cancel(current.timer);
          current = null;
          reject(signal.reason);
        }, {once: true});
        tick();
      });
    },
    /** Try at once, and count down anew from the next failure. */
    tryNow: () => fire(),
    /** The page got in: the screen goes. */
    done() {
      if (current) cancel(current.timer);
      current = null;
      view.hide();
    },
    /** Stop listening to the window; the screen stays as it is. */
    dispose() {
      if (current) cancel(current.timer);
      window?.removeEventListener?.('online', onOnline);
      window?.removeEventListener?.('offline', onOffline);
      document?.removeEventListener?.('visibilitychange', onVisible);
    },
  };
}

/** What the screen says for each reason: [heading, heading without the
 * workspace's name, body], each a locale message id and its English. */
const COPY = {
  unreachable: [['arrival-reconnect-unreachable', 'Can’t reach { $workspace }'], ['arrival-reconnect-unreachable-unnamed', 'Can’t reach the workspace'],
    ['arrival-reconnect-unreachable-body', 'It may be restarting. It opens here as soon as it’s back.']],
  refused: [['arrival-reconnect-refused', 'Not in { $workspace } yet'], ['arrival-reconnect-refused-unnamed', 'Not in the workspace yet'],
    ['arrival-reconnect-refused-body', 'It turned this browser away for now. The next try asks as a new device of yours.']],
  offline: [['arrival-reconnect-offline', 'You’re offline'], ['arrival-reconnect-offline', 'You’re offline'],
    ['arrival-reconnect-offline-body', 'The workspace opens as soon as you’re back online.']],
  busy: [['arrival-reconnect-busy', '{ $workspace } is busy'], ['arrival-reconnect-busy-unnamed', 'The workspace is busy'],
    ['arrival-reconnect-busy-body', 'A lot of people are getting in right now. It opens here once there’s room.']],
};

/**
 * The reconnect screen in `document` (index.html `#arrival-reconnect`),
 * worded from `messages()` (the locale's), naming `workspace()`. Shown
 * while `data-reconnect` is on the root. `onTryNow` runs on "Try now".
 */
export function domReconnectView({document = globalThis.document, messages = () => new Map(), workspace = () => null,
  onTryNow = () => {}, clipboard = globalThis.navigator?.clipboard} = {}) {
  const $ = id => document.getElementById(id);
  const root = document.documentElement;
  const say = (id, english, args) => localized(messages(), id, english, args);
  const set = (id, text) => { const element = $(id); if (element && element.textContent !== text) element.textContent = text; };
  let details = '';
  let announced = null;
  $('arrival-reconnect-now')?.addEventListener('click', () => onTryNow());
  $('arrival-reconnect-copy')?.addEventListener('click', async () => {
    const button = $('arrival-reconnect-copy');
    try {
      await clipboard.writeText(details);
      button.textContent = say('arrival-reconnect-copied', 'Copied');
    } catch {
      button.textContent = say('arrival-reconnect-copy-failed', 'Couldn’t copy. Select the text instead.');
    }
  });
  return {
    show(state) {
      const named = workspace();
      const [heading, unnamed, body] = COPY[state.reason] ?? COPY.unreachable;
      const title = named ? say(heading[0], heading[1], {workspace: named}) : say(unnamed[0], unnamed[1]);
      // A host on a machine's own address that never answers may be one
      // whose certificate this browser refuses: waiting does not fix that,
      // and only the browser can say which it is.
      const local = state.reason === 'unreachable' && localHost(state.origin);
      const words = local
        ? say('arrival-reconnect-unreachable-local-body', '{ $host } may be restarting, or this browser doesn’t trust its certificate. Open it to see which; the workspace opens here as soon as it answers.', {host: hostName(state.origin)})
        : say(body[0], body[1]);
      const opener = $('arrival-reconnect-host');
      if (opener) {
        opener.hidden = !local;
        if (local) {
          opener.href = state.origin;
          opener.textContent = say('entry-failure-open-host', 'Open { $host }', {host: hostName(state.origin)});
        }
      }
      const next = state.waiting === 'trying' ? say('arrival-reconnect-trying', 'Trying now…')
        : state.waiting === 'offline' ? say('arrival-reconnect-waiting-online', 'Waiting for the network…')
          : say('arrival-reconnect-next', 'Trying again in { $time }', {time: countdownText(state.remainingMs)});
      set('arrival-reconnect-heading', title);
      set('arrival-reconnect-body', words);
      set('arrival-reconnect-next', next);
      // Said to screen readers when the case or the step changes, not on
      // every second of the countdown.
      const said = `${title}. ${words}`;
      if (announced !== `${said}|${state.waiting}`) {
        announced = `${said}|${state.waiting}`;
        set('arrival-reconnect-said', state.waiting === 'trying' ? next : said);
      }
      const button = $('arrival-reconnect-now');
      if (button) {
        button.disabled = state.waiting === 'trying';
        button.setAttribute('aria-busy', String(state.waiting === 'trying'));
      }
      set('arrival-reconnect-now-label', say('arrival-reconnect-try-now', 'Try now'));
      set('arrival-reconnect-summary', say('arrival-reconnect-details', 'Technical details'));
      if (details !== (details = reconnectDetails(state))) {
        set('arrival-reconnect-text', details);
        set('arrival-reconnect-copy', say('arrival-reconnect-copy', 'Copy details'));
      }
      root.dataset.reconnect = state.reason;
    },
    hide() {
      delete root.dataset.reconnect;
      announced = null;
    },
  };
}
