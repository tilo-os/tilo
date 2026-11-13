/**
 * The entry page's one failure screen: what stopped the page, in plain
 * words, the one thing that gets the person in (trying again only where
 * that can help, and saying why), and their other workspaces as a second
 * choice on the same screen. Nothing else is shown over it: the page has
 * one failure at a time, said in one place.
 *
 * `failureCause` names the case from what the page knows (the error, the
 * host it was reaching, how far it got, whether the browser is online and
 * whether this site still answers); `failureScreen` words it;
 * `domFailureView` shows it (index.html, `#arrival-failure`).
 */
import {localized} from './arrival.mjs';
import {describeError, hostName, localHost, redact} from './reconnect.mjs';

export {hostName, localHost};

/** Whether `error` is a request that never got an answer, or a connection
 * that dropped: the network, a host that is down, or a certificate the
 * browser refused (a browser tells a page these apart in no way). */
export function unanswered(error) {
  if (error?.name === 'HostAdmissionError' && error.code === 'unavailable') return true;
  return /^(fetch failed|Failed to fetch|NetworkError|Load failed)|: (Failed to fetch|NetworkError|Load failed)|WebSocket|connection (closed|reset|refused|lost)|timed? ?out/i
    .test(String(error?.message ?? ''));
}

/** The origin of the address `error` names in its words (a fetch or an
 * import that failed says which), or `null`. */
export function namedOrigin(error) {
  const address = String(error?.message ?? '').match(/https?:\/\/[^\s'"),]+/)?.[0];
  try { return address ? new URL(address).origin : null; } catch { return null; }
}

/** Whether `error` is a host's own answer that it could not do it: an HTTP
 * error status it replied with. */
function answeredWithError(error) {
  return Number.isInteger(error?.status) && error.status >= 400 || /\bHTTP [45]\d\d\b/.test(String(error?.message ?? ''));
}

/**
 * The case of a failure:
 * - `removed`, `cooling-down`, `invitation-only`: the host's door turned
 *   the person away;
 * - `gone`, `deleted`, `no-access`, `refused`: the workspace is not there
 *   for this browser any more, or the link or invitation could not be used
 *   (`refused` says the page's own words for it);
 * - `custody-busy`: every device this browser keeps is open in other tabs;
 * - `offline`: the browser itself is offline;
 * - `unreachable`: the host never answered, while this site does (so the
 *   connection works: the host is down, or, for a host on a machine's own
 *   address, the browser refused its certificate); `no-connection`: neither
 *   answered; a file of this site's own that did not come while the site
 *   answers is no host that is down (`hostIsSite`), and reads as below;
 * - `host-silent`: the page reached the host, which never let it in, or
 *   answered that it does not (`not-let-in`);
 * - `host-error`: the host answered with an error;
 * - `desktop`: the host let the person in and the desktop did not start;
 *   `stopped`: the desktop stopped after it was drawn;
 * - `loading`: anything else before the page got in.
 *
 * `settled` is what the page found out about a workspace it remembers
 * (`gone`, `deleted`), `admitted` whether the host let the person in,
 * `drawn` whether the desktop had shown, `online` the browser's own
 * answer, `siteAnswers` whether this site answered once the page stopped.
 */
export function failureCause(error, {settled = null, admitted = false, drawn = false, online = true, siteAnswers = true, hostIsSite = false} = {}) {
  if (settled === 'gone' || settled === 'deleted') return settled;
  const code = error?.code;
  const fromDoor = ['AdmissionError', 'HostAdmissionError'].includes(error?.name);
  if (fromDoor && ['removed', 'invitation-only', 'cooling-down'].includes(code)) return code;
  if (error?.name === 'AdmissionError') return code === 'no-access' ? 'no-access' : 'refused';
  if (error?.name === 'CustodyError' && code === 'custody-busy') return 'custody-busy';
  if (drawn) return 'stopped';
  // Connected, and the host never let the page in, or said it does not.
  if (error?.code === 'not-let-in' || error?.name === 'TimeoutError' && error.connected) return 'host-silent';
  if (unanswered(error)) {
    if (online === false) return 'offline';
    if (!siteAnswers) return 'no-connection';
    // This site's own file that did not come while the site answers: a
    // moment's failure, not a host that is down.
    if (!hostIsSite) return 'unreachable';
  }
  if (answeredWithError(error)) return 'host-error';
  return admitted ? 'desktop' : 'loading';
}

/** Whether trying again can change the outcome of a `cause`: never for a
 * host's answer about this person or a workspace that is gone. */
export function retryHelps(cause) {
  return !['removed', 'invitation-only', 'cooling-down', 'gone', 'deleted', 'no-access', 'refused'].includes(cause);
}

/** Each case's English heading and body, with and without the workspace's
 * name; the locale file words them as `entry-failure-<cause>[-unnamed]` and
 * `entry-failure-<cause>-body`. `{ $host }` is the host's address. */
export const SCREENS = {
  removed: ['You were removed from { $workspace }', 'You were removed from this workspace',
    'To come back, ask someone in it for a new invitation.'],
  'cooling-down': ['{ $workspace } isn’t letting your network in', 'This workspace isn’t letting your network in',
    'Someone on your network was removed from it. Ask someone in it for an invitation, or try again tomorrow.'],
  'invitation-only': ['{ $workspace } is invite-only for now', 'This workspace is invite-only for now',
    'Ask someone in it for an invite link, then open it here.'],
  gone: ['{ $workspace } no longer exists', 'This workspace no longer exists',
    'Its host was set up again since this browser last opened it.'],
  deleted: ['{ $workspace } was deleted', 'This workspace was deleted',
    'Its owner deleted it, so it can’t be opened anymore.'],
  'no-access': ['{ $workspace } didn’t let this browser in', 'This workspace didn’t let this browser in',
    'You may have been removed from it, or it no longer exists.'],
  refused: ['Couldn’t open { $workspace }', 'Couldn’t open this workspace', '{ $reason }'],
  'custody-busy': ['{ $workspace } is open in your other tabs', 'This workspace is open in your other tabs',
    'This browser can open it in only so many tabs at once. Close one of the others, then try again.'],
  offline: ['You’re offline', 'You’re offline',
    'The workspace opens here as soon as you’re back online.'],
  unreachable: ['Can’t connect to { $host }', 'Can’t connect to { $host }',
    'This site loads, so your connection works. The host may be restarting: try again in a moment.'],
  'unreachable-local': ['Can’t connect to { $host }', 'Can’t connect to { $host }',
    'This site loads, so your connection works. Either the host is down, or this browser doesn’t trust its certificate. Open { $host } to see which, then try again.'],
  'no-connection': ['Can’t connect to { $host }', 'Can’t connect to { $host }',
    'This site doesn’t answer either. Check your connection, then try again.'],
  'host-silent': ['{ $host } didn’t let this tab into { $workspace }', '{ $host } didn’t let this tab in',
    'The host answered, but didn’t let this tab in. Try again. If it stops again, copy the technical details below and send them to whoever runs this site.'],
  'host-error': ['{ $host } couldn’t open { $workspace }', '{ $host } couldn’t open this workspace',
    'It answered with an error. If it was restarting, trying again in a moment opens it.'],
  desktop: ['{ $workspace } didn’t start', 'This workspace didn’t start',
    '{ $step } Your windows and documents are kept in this browser, so trying again picks up where you left off.'],
  stopped: ['{ $workspace } stopped working', 'The desktop stopped working',
    'Your windows and documents are kept in this browser, so trying again picks up where you left off.'],
  loading: ['Couldn’t open { $workspace }', 'Couldn’t open Tilo',
    '{ $step } Try again. If it stops again, copy the technical details below and send them to whoever runs this site.'],
};

/** Where the page stopped, by its arrival step (`entry-page.mjs`), as the
 * body of `desktop` and `loading` says it. */
const STEPS = {
  load: ['entry-failure-step-load', 'It stopped while loading Tilo.'],
  join: ['entry-failure-step-join', 'It stopped while joining.'],
  open: ['entry-failure-step-open', 'It stopped while setting up its desktop.'],
  draw: ['entry-failure-step-draw', 'It stopped while drawing its first screen.'],
};

/**
 * The screen for `cause`: `{cause, heading, body, retry, openHost}` in
 * `messages`' words. `retry` is whether it offers Try again; `openHost` the
 * host's origin to open, where only that tells the person what is wrong.
 */
export function failureScreen(cause, {workspace = null, host = null, step = null, error = null, messages = null} = {}) {
  const local = cause === 'unreachable' && host && localHost(host);
  const key = local ? 'unreachable-local' : (SCREENS[cause] ? cause : 'loading');
  const [named, unnamed, body] = SCREENS[key];
  const say = (id, english, args) => localized(messages, id, english, args);
  const address = hostName(host) ?? say('entry-failure-the-host', 'the host');
  const [stepId, stepEnglish] = STEPS[step] ?? STEPS.load;
  const args = {workspace, host: address, step: say(stepId, stepEnglish),
    reason: String(error?.message ?? '').trim() || say('entry-failure-refused-fallback', 'This link or invitation can’t be used.')};
  return {
    cause,
    heading: workspace ? say(`entry-failure-${key}`, named, args) : say(`entry-failure-${key}-unnamed`, unnamed, args),
    body: say(`entry-failure-${key}-body`, body, args),
    retry: retryHelps(cause),
    openHost: local ? host : null,
  };
}

/** The technical details of a failure, to read or copy: what the page
 * makes of it, the host, the step, the error and the page's build. Never a
 * token, key or certificate. */
export function failureDetails({cause, host = null, step = null, error = null, build = null}) {
  return [
    `Cause: ${cause}`,
    `Host: ${host ? (hostName(host) ?? 'unknown') : 'none'}`,
    `Stopped at: ${step ?? 'unknown'}`,
    `Error: ${describeError(error)}`,
    `Page build: ${redact(build ?? 'unknown')}`,
  ].join('\n');
}

/**
 * The failure screen in `document` (index.html, `#arrival-failure`), shown
 * while `data-failure` is on the root. `show({screen, details, others})`
 * fills it: `others` are `{label, note, open}` for the person's other workspaces
 * (their own desktop among them), listed under the screen's own action.
 * `onRetry` runs on Try again.
 */
export function domFailureView({document = globalThis.document, messages = () => new Map(), onRetry = () => {},
  open = url => globalThis.open?.(url, '_blank', 'noopener'), clipboard = globalThis.navigator?.clipboard} = {}) {
  const $ = id => document.getElementById(id);
  const root = document.documentElement;
  const say = (id, english, args) => localized(messages(), id, english, args);
  const set = (id, text) => { const element = $(id); if (element) element.textContent = text; };
  let details = '';
  let host = null;
  $('arrival-failure-retry')?.addEventListener('click', () => onRetry());
  $('arrival-failure-host')?.addEventListener('click', () => { if (host) open(host); });
  $('arrival-failure-copy')?.addEventListener('click', async () => {
    try {
      await clipboard.writeText(details);
      set('arrival-failure-copy', say('arrival-reconnect-copied', 'Copied'));
    } catch {
      set('arrival-failure-copy', say('arrival-reconnect-copy-failed', 'Couldn’t copy. Select the text instead.'));
    }
  });
  return {
    show({screen, details: text = '', others = []}) {
      details = text;
      host = screen.openHost;
      set('arrival-failure-heading', screen.heading);
      set('arrival-failure-body', screen.body);
      const retry = $('arrival-failure-retry');
      const opener = $('arrival-failure-host');
      // The one action that gets the person in leads: opening the host where
      // only the browser can say what is wrong with it, else trying again.
      if (opener) {
        opener.hidden = !host;
        opener.textContent = host ? say('entry-failure-open-host', 'Open { $host }', {host: hostName(host)}) : '';
        opener.className = host ? 'failure-primary' : '';
      }
      if (retry) {
        retry.hidden = !screen.retry;
        retry.textContent = say('arrival-retry', 'Try again');
        retry.className = host ? 'failure-secondary' : 'failure-primary';
      }
      const list = $('arrival-failure-others');
      if (list) {
        list.replaceChildren(...others.map(other => {
          const item = document.createElement('li');
          const button = document.createElement('button');
          button.type = 'button';
          const name = document.createElement('span');
          name.textContent = other.label;
          button.append(name);
          if (other.note) {
            const note = document.createElement('span');
            note.className = 'failure-note';
            note.textContent = other.note;
            button.append(note);
          }
          button.addEventListener('click', () => other.open(), {once: true});
          item.append(button);
          return item;
        }));
      }
      set('arrival-failure-others-heading', others.length ? say('entry-failure-others', 'Or open another of your workspaces') : '');
      const group = $('arrival-failure-elsewhere');
      if (group) group.hidden = !others.length;
      set('arrival-failure-summary', say('arrival-reconnect-details', 'Technical details'));
      set('arrival-failure-text', text);
      set('arrival-failure-copy', say('arrival-reconnect-copy', 'Copy details'));
      const detailsView = $('arrival-failure-details');
      if (detailsView) detailsView.hidden = !text;
      // Said to screen readers once, in the page's status line too.
      set('status', `${screen.heading}. ${screen.body}`);
      root.dataset.failure = screen.cause;
      // The keyboard goes to the action that gets the person in.
      (host ? opener : screen.retry ? retry : null)?.focus?.();
    },
    hide() {
      delete root.dataset.failure;
    },
  };
}
