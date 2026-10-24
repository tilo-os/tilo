/**
 * The arrival screen's presentation (index.html, `#arrival`): while the
 * desktop is on its way, a thin progress bar moved by the real steps of
 * getting in, and above it one rotating line, a joke or a tip, from the
 * locale files beside this module (`locales/<locale>.ftl`), in the
 * desktop's language.
 *
 * What it reads is what the page already says: `data-arrival` on the root
 * (`load`, `join`, `open`, `draw`, `done`, `stopped`; entry-page.mjs) and the
 * runtime module's fetch in the browser's resource timing. The step notes
 * the page writes to `#status` ("Loading Tilo…", "Joining…") stay for
 * screen readers and leave the screen; anything else said there (a
 * pairing code, waiting for the owner, a failure) is shown in plain words
 * in the line's place, and no joke runs beside it. Nothing shows in the
 * first second, so a fast load never flashes. Steps and their timings go
 * to the console for debugging.
 */

/** Seconds each phase takes, measured on warm loads: the share of the bar
 * it gets. The page cannot see where its own JavaScript ends and the
 * runtime's fetch begins, nor the GPU's set-up end before the first frame,
 * so those pairs are one segment each. */
export const PHASE_SECONDS = {js: 0.8, fetch: 1.3, compile: 0.45, join: 1.3, open: 1.4, gpu: 1.6, frame: 0.3};
const SEGMENT_PHASES = [
  ['fetch', ['js', 'fetch']],
  ['compile', ['compile']],
  ['join', ['join']],
  ['open', ['open']],
  ['draw', ['gpu', 'frame']],
];
const TOTAL = Object.values(PHASE_SECONDS).reduce((sum, seconds) => sum + seconds, 0);
/** Each segment's `[start, end]` on the bar (0…1) and expected seconds. */
export const SEGMENTS = (() => {
  let at = 0;
  return Object.fromEntries(SEGMENT_PHASES.map(([name, phases]) => {
    const seconds = phases.reduce((sum, phase) => sum + PHASE_SECONDS[phase], 0);
    const segment = {start: at / TOTAL, end: (at + seconds) / TOTAL, seconds};
    at += seconds;
    return [name, segment];
  }));
})();

/** The bar's segment for the page's step; `load` splits at the runtime
 * module's arrival. */
export function segmentOf(arrival, wasmFetched) {
  if (arrival === 'load') return wasmFetched ? 'compile' : 'fetch';
  return arrival in SEGMENTS || arrival === 'done' || arrival === 'stopped' ? arrival : 'fetch';
}

/** How far the bar stands `elapsed` ms into `segment`: from the segment's
 * start creeping toward its end, never reaching it. Reduced motion steps:
 * the segment's start only. */
export function progressIn(segment, elapsed, {reducedMotion = false} = {}) {
  if (segment === 'done') return 1;
  const span = SEGMENTS[segment];
  if (!span) return 0;
  if (reducedMotion) return span.start;
  const creep = 0.9 * (1 - Math.exp(-Math.max(0, elapsed) / (span.seconds * 1000)));
  return span.start + (span.end - span.start) * creep;
}

/** The strings of a Fluent file: `id = value` messages on one line each.
 * The arrival lines need nothing more; anything else is skipped. */
export function parseMessages(text) {
  const messages = new Map();
  for (const line of String(text).split('\n')) {
    const match = /^([A-Za-z][A-Za-z0-9_-]*)\s*=\s*(.*?)\s*$/.exec(line);
    if (match && match[2]) messages.set(match[1], match[2]);
  }
  return messages;
}

/** Whether a page at `hostname` is on tilo.coffee, the hosted Lounge,
 * whose own lines (`arrival-coffee-*`) are shown only there. */
export function isCoffeeHost(hostname) {
  const host = String(hostname ?? '').toLowerCase().replace(/\.$/, '');
  return host === 'tilo.coffee' || host.endsWith('.tilo.coffee');
}

/**
 * The jokes and tips from `messages` for this page: `arrival-joke-*` for
 * everyone, `arrival-coffee-*` only on tilo.coffee, `arrival-tip-*` in both
 * modes, `arrival-desktop-tip-*` only on the desktop. A tip's `-mac`
 * message replaces it on a Mac.
 */
export function arrivalLines(messages, {mode = 'desktop', mac = false, hostname = ''} = {}) {
  const jokes = [];
  const tips = [];
  for (const [id, value] of messages) {
    if (id.endsWith('-mac')) continue;
    const mine = mac && messages.has(`${id}-mac`) ? messages.get(`${id}-mac`) : value;
    if (id.startsWith('arrival-joke-')) jokes.push(mine);
    else if (id.startsWith('arrival-coffee-') && isCoffeeHost(hostname)) jokes.push(mine);
    else if (id.startsWith('arrival-tip-')) tips.push(mine);
    else if (id.startsWith('arrival-desktop-tip-') && mode === 'desktop') tips.push(mine);
  }
  return {jokes, tips};
}

/**
 * The lines of one visit, in random order and none twice: two jokes, then
 * a tip, opening with a joke. A pool that runs out leaves its turns to the
 * other; once both are spent, `next()` is `null` and the last line stays.
 * Each line is `{kind: 'joke' | 'tip', text}`.
 */
export function createRotation({jokes, tips}, random = Math.random) {
  const shuffled = list => {
    const out = [...list];
    for (let i = out.length - 1; i > 0; i--) {
      const j = Math.floor(random() * (i + 1));
      [out[i], out[j]] = [out[j], out[i]];
    }
    return out;
  };
  const pools = {joke: shuffled(jokes).map(text => ({kind: 'joke', text})),
    tip: shuffled(tips).map(text => ({kind: 'tip', text}))};
  let turn = 0;
  return {
    next() {
      const want = turn % 3 === 2 ? 'tip' : 'joke';
      const other = want === 'tip' ? 'joke' : 'tip';
      const line = pools[want].shift() ?? pools[other].shift() ?? null;
      if (line !== null) turn += 1;
      return line;
    },
  };
}

/** The notes the page writes for its own steps: kept for screen readers,
 * never shown, since the bar says the same. Anything else in `#status` is
 * meant to be read. */
export const STEP_NOTES = new Set([
  'Loading Tilo…', 'Opening the workspace…', 'Opening the app…', 'Starting graphics…',
  'Connecting…', 'Asking to join…', 'Joining…', 'Asking to pair this device…',
]);

/** Said in place of the line when a step that waits on the network, or on
 * the GPU's first frame, runs long; the locale file may word them. */
export const SLOW_NOTES = {
  fetch: ['arrival-slow-loading', 'Still loading. Your network may be slow.'],
  join: ['arrival-slow-connecting', 'Still connecting. Your network may be slow.'],
  draw: ['arrival-slow-drawing', 'Still starting graphics. Without a graphics card this takes longer.'],
};

/**
 * What the arrival screen shows at `now` (ms since the page started):
 * `{line, note, progress, bar}`. Feed it the page's state on every tick
 * (`{arrival, wasmFetched, statusShown}`; `statusShown` when `#status`
 * says something other than a step note). Lines rotate every `periodMs`
 * from `delayMs`; before that, nothing. A failed or stopped page shows no
 * line, note or bar: its status says what went wrong.
 */
export function createArrival({lines = {jokes: [], tips: []}, random = Math.random, reducedMotion = false,
  delayMs = 1000, periodMs = 2500, slowMs = {fetch: 8000, join: 6000, draw: 10_000}, slowNotes = {}} = {}) {
  let rotation = createRotation(lines, random);
  let line = null;
  let shownAt = null;
  let segment = null;
  let since = 0;
  let progress = 0;
  return {
    /** New lines (the locale file arrived): the next turn draws from them. */
    setLines(next) { rotation = createRotation(next, random); },
    get segment() { return segment; },
    update({arrival, wasmFetched = false, statusShown = false}, now) {
      const current = segmentOf(arrival, wasmFetched);
      // The first segment runs from the page's start, before this module.
      if (current !== segment) { since = segment === null ? 0 : now; segment = current; }
      if (segment === 'stopped') return {line: null, note: null, progress, bar: false};
      // Never backwards: a skipped step (no door to join) jumps forward.
      progress = Math.max(progress, progressIn(segment, now - since, {reducedMotion}));
      if (segment === 'done' || now < delayMs) return {line: null, note: null, progress, bar: false};
      const slow = slowMs[segment] !== undefined && now - since >= slowMs[segment] && !statusShown;
      const note = slow ? (slowNotes[segment] ?? SLOW_NOTES[segment][1]) : null;
      if (statusShown || slow) return {line: null, note, progress, bar: true};
      if (shownAt === null || now - shownAt >= periodMs) {
        const next = rotation.next();
        if (next !== null || line === null) { line = next; shownAt = next === null ? null : now; }
      }
      return {line, note: null, progress, bar: true};
    },
  };
}

/** How a line reads on screen: a tip after its dim label, a joke bare. */
export function lineParts(line, tipLabel = 'Tip') {
  return {label: line?.kind === 'tip' ? tipLabel : null, text: line?.text ?? ''};
}

/** Said when the desktop stops drawing; the cause goes to the console.
 * The locale file words it as `arrival-failed-stopped-drawing`. */
export const STOPPED_DRAWING = 'The desktop stopped working. Try again to start it.';

/** `text` with each `{ $name }` filled from `args`; a placeholder `args`
 * lacks stays as written. */
export function fill(text, args = {}) {
  return String(text).replace(/\{\s*\$([A-Za-z][A-Za-z0-9_-]*)\s*\}/g,
    (placeholder, name) => (Object.hasOwn(args, name) ? String(args[name]) : placeholder));
}

/** Message `id` of `messages` (a locale file's), filled from `args`, or the
 * English `fallback` where the locale lacks it. */
export function localized(messages, id, fallback, args) {
  return fill(messages?.get(id) ?? fallback, args);
}

/** What a failure says, by case: the locale file's `arrival-failed-<case>`
 * (naming the workspace) or `arrival-failed-<case>-unnamed` (where the page
 * does not know its name), else the English here. */
export const FAILURES = {
  'custody-busy': [
    'Every device this browser keeps for { $workspace } is open in another tab. Close one of those tabs, then try again.',
    'Every device this browser keeps for the workspace is open in another tab. Close one of those tabs, then try again.',
  ],
  removed: [
    'You were removed from { $workspace }. To come back, ask someone in it for a new invitation.',
    'You were removed from this workspace. To come back, ask someone in it for a new invitation.',
  ],
  'invitation-only': [
    'You need an invitation to join { $workspace } for now. Ask someone in it for an invite link, then open it here.',
    'You need an invitation to join this workspace for now. Ask someone in it for an invite link, then open it here.',
  ],
  'cooling-down': [
    '{ $workspace } is not letting anyone in from your network for now, because someone here was removed from it. Ask someone in it for an invitation, or try again tomorrow.',
    'This workspace is not letting anyone in from your network for now, because someone here was removed from it. Ask someone in it for an invitation, or try again tomorrow.',
  ],
  unreachable: [
    'Can’t reach { $workspace } right now. Check your connection, then try again.',
    'Can’t reach the workspace right now. Check your connection, then try again.',
  ],
  desktop: [
    'You got into { $workspace }, but its desktop did not start in this tab. Try again.',
    'You got into the workspace, but its desktop did not start in this tab. Try again.',
  ],
  general: [
    'Something went wrong opening { $workspace }. Try again, or come back later.',
    'Something went wrong opening the workspace. Try again, or come back later.',
  ],
};

/** Failure `kind` (a key of `FAILURES`) in `messages`' words. */
function failure(kind, workspace, messages) {
  const [named, unnamed] = FAILURES[kind];
  return workspace
    ? localized(messages, `arrival-failed-${kind}`, named, {workspace})
    : localized(messages, `arrival-failed-${kind}-unnamed`, unnamed);
}

/** Whether `error` is a host's door turning this visitor away for a while
 * (`cooling-down`: the address someone removed from the room joined from;
 * `removed`: the person themselves; `invitation-only`: a room that takes
 * people by invitation, and this visitor came without one),
 * which trying again does not change. */
export function refusedByHost(error) {
  return error?.name === 'HostAdmissionError' && ['cooling-down', 'removed', 'invitation-only'].includes(error.code);
}

/**
 * The words of the door a page turned away for coming without an
 * invitation shows (index.html, `#arrival-door`), in `messages`' language.
 * A door whose host keeps a wait list (`waitList`) is the product's own
 * front door: it apologises, says why, and asks for an address. Without
 * one, it names the workspace and how to get an invite.
 */
export function doorCopy({workspace = null, waitList = false, messages = null} = {}) {
  const say = (key, english, args) => localized(messages, key, english, args);
  if (waitList) {
    return {
      heading: say('arrival-door-wait-heading', 'Sorry, Tilo is invite-only right now'),
      body: say('arrival-door-wait-body', 'It’s an early alpha. Leave your email and we’ll send you an invite when there’s room.'),
    };
  }
  return {
    heading: workspace
      ? say('arrival-door-heading', '{ $workspace } is invite-only for now', {workspace})
      : say('arrival-door-heading-unnamed', 'This workspace is invite-only for now'),
    body: say('arrival-door-body', 'Ask someone in it for an invite link, then paste it here.'),
  };
}

/** What the door says when it kept no address, by `HostAdmissionError`
 * code (`leaveOnWaitList`), in `messages`' language. */
export function waitListFailure(code, messages = null) {
  const say = (key, english) => localized(messages, key, english);
  switch (code) {
    case 'invalid-email': return say('arrival-wait-invalid', 'That doesn’t look like an email address.');
    case 'too-many': return say('arrival-wait-too-many', 'Too many sign-ups from your network just now. Try again later.');
    case 'wait-list-full': return say('arrival-wait-full', 'The wait list is full for now. Please try again in a few days.');
    default: return say('arrival-wait-failed', 'That didn’t go through. Try again in a moment.');
  }
}

/** The page to open for `text`, an invite link a person pasted, or `null`
 * when it is not one: a web address carrying an invitation. */
export function inviteLinkTarget(text, base = globalThis.location?.href) {
  let url;
  try {
    url = new URL(String(text ?? '').trim(), base);
  } catch {
    return null;
  }
  if (!['https:', 'http:'].includes(url.protocol)) return null;
  const carries = part => new URLSearchParams(part.slice(part.indexOf('?') + 1)).has('invite');
  return (url.hash.includes('?') && carries(url.hash)) || carries(url.search) ? url.href : null;
}

/** Whether a page that stopped had got in: the host let the person in
 * (`admitted`, the peer) and the desktop had not drawn yet (`arrival`, the
 * page's arrival step). A desktop whose first frame never came marks the
 * arrival `stopped` itself before the page hears its mount failed: that is
 * still a desktop that did not start, not something that went wrong
 * getting in. */
export function stoppedInside(admitted, arrival) {
  return Boolean(admitted) && arrival !== 'done';
}

/** Whether `error` is the network failing: a fetch that never answered, a
 * host's door that refused or failed, a connection that dropped. */
function unreachable(error) {
  if (error?.name === 'HostAdmissionError') return true;
  return /fetch|network|connect|timed? ?out|websocket|offline|HTTP \d{3}|load failed/i
    .test(String(error?.message ?? error ?? ''));
}

/**
 * What a page that could not get in says, in plain words, beside its Try
 * again button, in the person's language (`messages`, the locale file the
 * arrival screen reads). A refusal says itself (it is written for people);
 * the network failing names the workspace where the page knows it (else
 * Tilo, while Tilo itself was still loading); a desktop that did not start
 * once the host let the person in (`admitted`) says so; anything else stays
 * general, its cause left to the console.
 */
export function failureMessage(error, {workspace = null, loaded = true, admitted = false, messages = null} = {}) {
  // Taken out of the room by its people: said in the person's words.
  if (['AdmissionError', 'HostAdmissionError'].includes(error?.name) && error.code === 'removed') {
    return failure('removed', workspace, messages);
  }
  // A room taking people by invitation, opened without one.
  if (['AdmissionError', 'HostAdmissionError'].includes(error?.name) && error.code === 'invitation-only') {
    return failure('invitation-only', workspace, messages);
  }
  if (error?.name === 'AdmissionError') return error.message;
  if (error?.name === 'CustodyError' && error.code === 'custody-busy') {
    // Each tab is a device of its own; this is only said where this tab
    // could not become one (every device key in use, and no way here to
    // certify another).
    return failure('custody-busy', workspace, messages);
  }
  if (refusedByHost(error)) return failure('cooling-down', workspace, messages);
  const name = workspace || (loaded ? null : 'Tilo');
  if (unreachable(error)) return failure('unreachable', name, messages);
  return failure(admitted ? 'desktop' : 'general', name, messages);
}

/**
 * What a page watching a room through its door says about joining, so it
 * never promises a join the door will not give. `entered`: `watching` (an
 * embed, until a gesture) or `seatless` (the door had no seat, `because`
 * its code); `presence`: the door's answer (`admission`: `open` or
 * `invite`; `joins`: `false` while nobody joins), `null` when it did not
 * answer. `join` is what the page does
 * next: `gesture` joins on a click or key, `seat` once the door has a seat
 * for a guest again, `none` never by itself.
 */
export function visitLine({entered, because = null, presence = null, workspace = null, messages = null}) {
  const embed = entered === 'watching';
  // Only members and invitees get in: the door's own words for it.
  if (embed && presence?.admission === 'invite') {
    return {line: `You’re watching live. ${failure('invitation-only', workspace, messages)}`, join: 'none'};
  }
  if (presence?.joins === false) {
    return embed
      ? {line: 'You’re watching live. Joining is closed for now; you can join once it opens.', join: 'seat'}
      : {line: 'Joining is closed for now, so you’re watching live. You’ll join once it opens.', join: 'seat'};
  }
  if (embed) return {line: 'Click or type to join', join: 'gesture'};
  return {
    line: because === 'full'
      ? 'It’s full right now, so you’re watching live. You’ll join when a seat frees up.'
      : 'You couldn’t join just now, so you’re watching live. You’ll join in a moment.',
    join: 'seat',
  };
}

/** The language the desktop speaks before the person chooses one in
 * Settings: its source language. The browser's own language is not it, so
 * the arrival screen and its failures do not follow the browser either: a
 * page in German that opens an English desktop reads as two products. */
export const DESKTOP_LANGUAGE = 'en-US';

/** The locale file for the desktop's `language`: German for German, else
 * English. */
export function arrivalLocale(language = DESKTOP_LANGUAGE) {
  return /^de\b/i.test(String(language ?? '')) ? 'de-DE' : 'en-US';
}

let pageMessages = null;
/** The messages of the desktop's locale file (`arrivalLocale`), fetched
 * once per page: the arrival screen's lines and what a failure says. An
 * empty map when the file cannot be read: everything has its English. */
export function localeMessages(window = globalThis, language = DESKTOP_LANGUAGE) {
  pageMessages ??= Promise.resolve()
    .then(() => window.fetch(new URL(`locales/${arrivalLocale(language)}.ftl`, import.meta.url)))
    .then(response => (response.ok ? response.text() : ''))
    .then(parseMessages)
    .catch(() => new Map());
  return pageMessages;
}

/** `localeMessages()`, or an empty map if it has not arrived within
 * `waitMs`: a failure is said at once, in English if need be. */
export function localeMessagesSoon(waitMs = 1500, window = globalThis) {
  let timer;
  const late = new Promise(resolve => { timer = setTimeout(() => resolve(new Map()), waitMs); });
  return Promise.race([localeMessages(window), late]).finally(() => clearTimeout(timer));
}

/**
 * Run the arrival screen on the page: reads `data-arrival` and `#status`,
 * writes the line (`#arrival-line`), the bar (`#arrival-bar`) and a slow
 * network's note (`#arrival-note`). Stops once the desktop is shown. Try
 * again (`#arrival-retry`, shown by index.html on a failure) reloads.
 */
export function startArrival({mode = 'desktop', document = globalThis.document, window = globalThis} = {}) {
  const root = document.documentElement;
  const lineView = document.getElementById('arrival-line');
  const barView = document.getElementById('arrival-bar');
  const noteView = document.getElementById('arrival-note');
  const status = document.getElementById('status');
  if (!lineView || !barView || !noteView || !status) return null;
  const retry = document.getElementById('arrival-retry');
  retry?.addEventListener('click', () => window.location.reload());
  const reducedMotion = Boolean(window.matchMedia?.('(prefers-reduced-motion: reduce)').matches);
  const mac = /Mac/.test(window.navigator?.platform ?? '');
  const slowNotes = {};
  let tipLabel = 'Tip';
  const arrival = createArrival({reducedMotion, slowNotes});
  const now = () => window.performance.now();
  let wasmFetched = false;
  let observer = null;
  try {
    observer = new window.PerformanceObserver(list => {
      if (!wasmFetched && list.getEntries().some(entry => /\.wasm(\?|$)/.test(entry.name))) { wasmFetched = true; tick(); }
    });
    observer.observe({type: 'resource', buffered: true});
  } catch { /* No resource timing: the load step is one segment. */ }
  void localeMessages(window)
    .then(messages => {
      arrival.setLines(arrivalLines(messages, {mode, mac, hostname: window.location?.hostname}));
      tipLabel = messages.get('arrival-label-tip') ?? tipLabel;
      for (const [segment, [id]] of Object.entries(SLOW_NOTES)) {
        if (messages.has(id)) slowNotes[segment] = messages.get(id);
      }
      if (retry && messages.has('arrival-retry')) retry.textContent = messages.get('arrival-retry');
    })
    .catch(() => { /* No lines: the bar alone. */ });
  let shownLine = null;
  let logged = null;
  let swap = null;
  const tick = () => {
    const step = root.dataset.arrival;
    const statusShown = status.textContent.trim() !== '' && !STEP_NOTES.has(status.textContent.trim());
    root.toggleAttribute('data-arrival-status', statusShown);
    const view = arrival.update({arrival: step, wasmFetched, statusShown}, now());
    if (arrival.segment !== logged) {
      logged = arrival.segment;
      console.debug(`arrival: ${logged} at ${Math.round(now())} ms`);
    }
    root.dataset.arrivalProgress = view.progress.toFixed(3);
    barView.hidden = !view.bar;
    barView.style.setProperty('--progress', view.progress.toFixed(4));
    const note = view.note ?? '';
    if (noteView.textContent !== note) noteView.textContent = note;
    noteView.hidden = !note;
    if (view.line !== shownLine) {
      shownLine = view.line;
      clearTimeout(swap);
      const show = () => {
        const {label, text} = lineParts(shownLine, tipLabel);
        const words = document.createTextNode(text);
        if (label) {
          const tag = document.createElement('span');
          tag.className = 'label';
          tag.textContent = label;
          lineView.replaceChildren(tag, words);
        } else {
          lineView.replaceChildren(words);
        }
        lineView.classList.toggle('shown', shownLine !== null);
      };
      if (reducedMotion || !lineView.textContent || view.line === null) {
        show();
      } else {
        // A crossfade: out, the new words, back in.
        lineView.classList.remove('shown');
        swap = setTimeout(show, 240);
      }
    }
    // A failure's one control takes the keyboard, unless the page holds it elsewhere.
    if (step === 'stopped' && retry && window.getComputedStyle?.(retry).display !== 'none'
      && [document.body, null].includes(document.activeElement)) retry.focus();
    if (step === 'done' || step === 'stopped') stop();
  };
  const timer = setInterval(tick, reducedMotion ? 250 : 120);
  const watch = new window.MutationObserver(tick);
  watch.observe(root, {attributes: true, attributeFilter: ['data-arrival']});
  watch.observe(status, {childList: true, characterData: true, subtree: true});
  function stop() {
    clearInterval(timer);
    watch.disconnect();
    observer?.disconnect();
  }
  tick();
  return {stop};
}
