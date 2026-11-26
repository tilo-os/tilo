/**
 * The arrival screen's presentation (index.html, `#arrival`): while the
 * desktop is on its way, a thin progress bar moved by the real steps of
 * getting in, and above it one line saying which step that is.
 *
 * What it reads is what the page already says: `data-arrival` on the root
 * (`load`, `join`, `open`, `draw`, `done`, `stopped`; entry-page.mjs) and the
 * runtime module's fetch in the browser's resource timing. The step notes
 * the page writes to `#status` ("Loading Tilo…", "Joining…") are the line;
 * anything else said there (a pairing code, waiting for the owner, a
 * failure) is shown in plain words in the line's place. Nothing shows in
 * the first second, so a fast load never flashes. Steps and their timings
 * go to the console for debugging.
 */

/** The steps the bar has, in the order a page goes through them. The page
 * reports them as `data-arrival` (`load` covers the first two, split by
 * whether the runtime module has been fetched). */
export const STEPS = ['fetch', 'compile', 'join', 'open', 'draw'];

/** Seconds each step takes on a device that has never loaded Tilo: what the
 * bar is sized by before anything has been measured here. Only a first visit
 * can read these; afterwards the bar is planned from what this device's last
 * arrival actually took (`measure`), and each step's own time replaces its
 * estimate as it completes, so the guesses are for the first visit alone. */
export const FIRST_VISIT_SECONDS = {fetch: 1.6, compile: 0.9, join: 1.1, open: 1.3, draw: 1.6};

/** `seconds` (a measurement or the first visit's) as each step's `[start,
 * end]` share of the bar and its seconds: what the bar is sized by. */
export function plannedSegments(seconds = FIRST_VISIT_SECONDS) {
  const total = STEPS.reduce((sum, step) => sum + Math.max(0, seconds[step] ?? 0), 0) || 1;
  let at = 0;
  return Object.fromEntries(STEPS.map(step => {
    const span = Math.max(0, seconds[step] ?? 0);
    const segment = {start: at / total, end: (at + span) / total, seconds: span};
    at += span;
    return [step, segment];
  }));
}

/** The bar's segments for a first visit. */
export const SEGMENTS = plannedSegments();

/** `measured`'s steps that are a real time in ms, all else left out. */
export function measuredSeconds(measured) {
  const out = {};
  for (const step of STEPS) {
    const value = measured?.[step];
    if (typeof value === 'number' && Number.isFinite(value) && value > 0) out[step] = value;
  }
  return out;
}

/** A measurement (ms, as kept) as the bar's plan: seconds by step. */
export function measuredPlan(measured) {
  return Object.fromEntries(Object.entries(measuredSeconds(measured)).map(([step, ms]) => [step, ms / 1000]));
}

/** Where this device's last arrival's step times are kept, so the next bar
 * is planned from what getting in actually took here. */
export const MEASURED_KEY = 'tilo-arrival-seconds';

/** This device's last arrival, by step (ms); empty when it has not had one,
 * or when the page may not keep anything. */
export function rememberedArrival(window = globalThis) {
  try {
    return measuredSeconds(JSON.parse(window.localStorage?.getItem(MEASURED_KEY) ?? 'null'));
  } catch {
    return {};
  }
}

/** Keep `spent` (this arrival's step times, ms) for the next one. */
export function rememberArrival(spent, window = globalThis) {
  const seconds = measuredSeconds(spent);
  if (Object.keys(seconds).length === 0) return;
  try {
    window.localStorage?.setItem(MEASURED_KEY, JSON.stringify(seconds));
  } catch { /* No storage: the next bar is planned by the first visit's guess. */ }
}

/** The steps' times as one line for the console: `fetch 210 · compile 70 …`. */
export function describeSpent(spent) {
  return STEPS.filter(step => spent?.[step] !== undefined)
    .map(step => `${step} ${Math.round(spent[step])} ms`).join(' · ');
}

/** The bar's segment for the page's step; `load` splits at the runtime
 * module's arrival. */
export function segmentOf(arrival, wasmFetched) {
  if (arrival === 'load') return wasmFetched ? 'compile' : 'fetch';
  return arrival in SEGMENTS || arrival === 'done' || arrival === 'stopped' ? arrival : 'fetch';
}

/** How far the bar stands `elapsed` ms into `step`, planned from `seconds`
 * (measured, or a first visit's): from the step's start creeping toward its
 * end, never reaching it, so the last step's creep ends where the arrival
 * does. Reduced motion steps: the step's start only. */
export function progressIn(step, elapsed, {reducedMotion = false, seconds = FIRST_VISIT_SECONDS} = {}) {
  if (step === 'done') return 1;
  const span = plannedSegments(seconds)[step];
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

/** The notes the page writes for its own steps: what the line says while
 * that step runs. Anything else in `#status` is said in the line's place,
 * apart from the bar's steps. */
export const STEP_NOTES = new Set([
  'Loading Tilo…', 'Opening the workspace…', 'Opening the app…',
  'Connecting…', 'Asking to join…', 'Joining…', 'Asking to pair this device…',
]);

/** Said in place of the line when a step runs long; the locale file may
 * word them. */
export const SLOW_NOTES = {
  fetch: ['arrival-slow-loading', 'Still loading Tilo.'],
  compile: ['arrival-slow-starting', 'Still starting Tilo.'],
  join: ['arrival-slow-connecting', 'Still connecting to the workspace.'],
  open: ['arrival-slow-opening', 'Still opening the workspace.'],
  draw: ['arrival-slow-drawing', 'Still starting graphics. Without a graphics card this takes longer.'],
};

/** Said, with Try again beside it, once a step has not moved on for long:
 * the page keeps waiting, since a busy device gets there in the end, and
 * the person may start over instead. */
export const STALLED = ['arrival-stalled', 'This is taking longer than it should. Keep waiting, or try again.'];

/**
 * What the arrival screen shows at `now` (ms since the page started):
 * `{line, note, progress, bar, stalled}`. Feed it the page's state on every
 * tick (`{arrival, wasmFetched, statusShown, gpuStalled}`; `statusShown`
 * when `#status` says something other than a step note, `stepNote` the step
 * note it says otherwise, `gpuStalled` when the runtime says graphics
 * start-up stopped moving on). The line is the step note from `delayMs`;
 * before that, nothing. A step that runs past
 * `slowMs` says so; one that has not moved on for `stallMs` (graphics:
 * once the runtime says so) is `stalled`, and the page offers Try again
 * while it keeps waiting. `slowNotes` may word each note (`stalled` too).
 * Nothing the page waits on a person for (being let in, a pairing code)
 * stalls. A failed or stopped page shows no line, note or bar: its status
 * says what went wrong.
 *
 * `measured` is what this device's last arrival took, by step
 * (`{fetch, compile, join, open, draw}` in ms, `measuredArrival`): the bar
 * is planned from those instead of a first visit's guess, and each step's
 * real time replaces its estimate as the step completes, so a bar planned
 * from a slow arrival does not stall on a fast one.
 */
export function createArrival({reducedMotion = false, measured = null, delayMs = 1000,
  slowMs = {fetch: 8000, compile: 8000, join: 6000, open: 8000, draw: 10_000},
  stallMs = 30_000, slowNotes = {}} = {}) {
  let segment = null;
  let since = 0;
  let progress = 0;
  // Each step's planned seconds: this device's measurement where there is
  // one, the first visit's guess elsewhere; a step that has run already says
  // what it took.
  let seconds = {...FIRST_VISIT_SECONDS, ...measuredPlan(measured)};
  const spent = {};
  return {
    get segment() { return segment; },
    /** The seconds each step is planned by now, measured ones included. */
    get seconds() { return seconds; },
    /** What this arrival's steps have taken so far, by step. */
    get spent() { return {...spent}; },
    update({arrival, wasmFetched = false, statusShown = false, stepNote = null, gpuStalled = false}, now) {
      const current = segmentOf(arrival, wasmFetched);
      // The first segment runs from the page's start, before this module.
      if (current !== segment) {
        if (segment !== null && STEPS.includes(segment)) {
          // The step's own time (ms, as kept) replaces its estimate (s).
          spent[segment] = Math.max(0, now - since);
          seconds = {...seconds, [segment]: spent[segment] / 1000};
        }
        since = segment === null ? 0 : now;
        segment = current;
      }
      if (segment === 'stopped') return {line: null, note: null, progress, bar: false, stalled: false};
      // Never backwards: a skipped step (no entry to join) jumps forward.
      progress = Math.max(progress, progressIn(segment, now - since, {reducedMotion, seconds}));
      if (segment === 'done') return {line: null, note: null, progress, bar: false, stalled: false};
      // Graphics start-up has steps of its own, which only the runtime sees.
      const stalled = !statusShown && (segment === 'draw' ? gpuStalled : now - since >= stallMs);
      if (now < delayMs) return {line: null, note: null, progress, bar: false, stalled};
      const slow = slowMs[segment] !== undefined && now - since >= slowMs[segment] && !statusShown;
      const note = stalled ? (slowNotes.stalled ?? STALLED[1]) : slow ? (slowNotes[segment] ?? SLOW_NOTES[segment][1]) : null;
      if (statusShown || slow || stalled) return {line: null, note, progress, bar: true, stalled};
      return {line: stepNote || null, note: null, progress, bar: true, stalled: false};
    },
  };
}

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

/** Whether `error` is a host's entry turning this person away for a while
 * (`cooling-down`: the address someone removed from the workspace joined from;
 * `removed`: the person themselves; `invitation-only`: a workspace that takes
 * people by invitation, and this person came without one),
 * which trying again does not change. */
export function refusedByHost(error) {
  return error?.name === 'HostAdmissionError' && ['cooling-down', 'removed', 'invitation-only'].includes(error.code);
}

/**
 * The words of the entry a page turned away for coming without an
 * invitation shows (index.html, `#arrival-refusal`), in `messages`' language.
 * An entry whose host keeps a wait list (`waitList`) is the product's own
 * front entry: it says what Tilo is, that it takes people by invitation
 * for now, and asks for an address. Without one, it names the workspace and
 * how to get an invite.
 */
export function refusalCopy({workspace = null, waitList = false, signIn = false, messages = null} = {}) {
  const say = (key, english, args) => localized(messages, key, english, args);
  // A link to a workspace only its people open, with nobody signed in here.
  if (signIn) {
    return {
      heading: workspace
        ? say('arrival-refusal-sign-in-heading', 'Sign in to open { $workspace }', {workspace})
        : say('arrival-refusal-sign-in-heading-unnamed', 'Sign in to open this workspace'),
      // Someone new can ask for an invite where the site keeps a wait list.
      body: waitList
        ? say('arrival-refusal-sign-in-wait-body', 'Only the people in this workspace can open it. If you’re one of them, sign in. New to Tilo? It’s invite-only right now: leave your email and we’ll send you an invite.')
        : say('arrival-refusal-sign-in-body', 'Only the people in this workspace can open it. Sign in as one of them, or paste an invite link someone sent you.'),
    };
  }
  if (waitList) {
    return {
      heading: say('arrival-refusal-wait-heading', 'One workspace for you, your team and your AI agents'),
      body: say('arrival-refusal-wait-body', 'Tilo is invite-only for now. Leave your email and we’ll send you an invite.'),
    };
  }
  return {
    heading: workspace
      ? say('arrival-refusal-heading', '{ $workspace } is invite-only for now', {workspace})
      : say('arrival-refusal-heading-unnamed', 'This workspace is invite-only for now'),
    body: say('arrival-refusal-body', 'Ask someone in it for an invite link, then paste it here. If you are in it on another device, paste the link “Pair a new device” makes there.'),
  };
}

/** What the entry says when it kept no address, by `HostAdmissionError`
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

/**
 * The page to open for `text`, a link a person pasted at the entry, or `null`
 * when it is not one that lets someone in. Such a link is a web address that
 * carries an invitation (`invite`: its own text, or the secret of the one a
 * workspace keeps at its host), or a pairing
 * (`pair`): the link "Pair a new device" makes on a device where the person
 * is signed in, which is all someone with no identity on a new device holds.
 * Whether the link is still good is for the page it opens to say.
 */
export function entryLinkTarget(text, base = globalThis.location?.href) {
  let url;
  try {
    url = new URL(String(text ?? '').trim(), base);
  } catch {
    return null;
  }
  if (!['https:', 'http:'].includes(url.protocol)) return null;
  const carries = part => {
    const secrets = new URLSearchParams(part.slice(part.indexOf('?') + 1));
    return secrets.has('invite') || secrets.has('pair');
  };
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

/** The locale file for the visitor's own language: the first of the
 * browser's languages this page has a catalogue for, German or English. The
 * entry's refusal shows no desktop, so it speaks for the person in front of
 * it rather than for a desktop that is not there. */
export function visitorLocale(window = globalThis) {
  const languages = window.navigator?.languages ?? [window.navigator?.language];
  const asked = languages.find(language => /^(de|en)\b/i.test(String(language ?? ''))) ?? '';
  return /^de\b/i.test(asked) ? 'de-DE' : 'en-US';
}

const pageMessages = new Map();
/** The messages of the locale file for `language` (`arrivalLocale`), fetched
 * once per page and locale: the arrival screen's lines and what a failure
 * says. An empty map when the file cannot be read: everything has its
 * English. */
export function localeMessages(window = globalThis, language = DESKTOP_LANGUAGE) {
  const locale = arrivalLocale(language);
  let messages = pageMessages.get(locale);
  if (!messages) {
    messages = Promise.resolve()
      .then(() => window.fetch(new URL(`locales/${locale}.ftl`, import.meta.url)))
      .then(response => (response.ok ? response.text() : ''))
      .then(parseMessages)
      .catch(() => new Map());
    pageMessages.set(locale, messages);
  }
  return messages;
}

/** `localeMessages()`, or an empty map if it has not arrived within
 * `waitMs`: a failure is said at once, in English if need be. */
export function localeMessagesSoon(waitMs = 1500, window = globalThis, language = DESKTOP_LANGUAGE) {
  let timer;
  const late = new Promise(resolve => { timer = setTimeout(() => resolve(new Map()), waitMs); });
  return Promise.race([localeMessages(window, language), late]).finally(() => clearTimeout(timer));
}

/**
 * Run the arrival screen on the page: reads `data-arrival` and `#status`,
 * writes the line (`#arrival-line`), the bar (`#arrival-bar`) and a slow
 * network's note (`#arrival-note`). Stops once the desktop is shown. Try
 * again (`#arrival-retry`, shown by index.html on a failure) reloads.
 */
export function startArrival({document = globalThis.document, window = globalThis} = {}) {
  const root = document.documentElement;
  const lineView = document.getElementById('arrival-line');
  const barView = document.getElementById('arrival-bar');
  const noteView = document.getElementById('arrival-note');
  const status = document.getElementById('status');
  if (!lineView || !barView || !noteView || !status) return null;
  const retry = document.getElementById('arrival-retry');
  retry?.addEventListener('click', () => window.location.reload());
  const reducedMotion = Boolean(window.matchMedia?.('(prefers-reduced-motion: reduce)').matches);
  const slowNotes = {};
  // Planned from what getting in took here last time, where there was one.
  const arrival = createArrival({reducedMotion, slowNotes, measured: rememberedArrival(window)});
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
      for (const [segment, [id]] of Object.entries(SLOW_NOTES)) {
        if (messages.has(id)) slowNotes[segment] = messages.get(id);
      }
      if (retry && messages.has('arrival-retry')) retry.textContent = messages.get('arrival-retry');
      if (messages.has(STALLED[0])) slowNotes.stalled = messages.get(STALLED[0]);
    })
    .catch(() => { /* No locale file: its notes in English. */ });
  let shownLine = null;
  let logged = null;
  let swap = null;
  const tick = () => {
    const step = root.dataset.arrival;
    const said = status.textContent.trim();
    const statusShown = said !== '' && !STEP_NOTES.has(said);
    root.toggleAttribute('data-arrival-status', statusShown);
    const view = arrival.update({arrival: step, wasmFetched, statusShown, stepNote: STEP_NOTES.has(said) ? said : null,
      gpuStalled: root.hasAttribute('data-gpu-stalled')}, now());
    // Try again shows beside a stalled step (index.html) while it keeps waiting.
    root.toggleAttribute('data-arrival-stalled', view.stalled);
    if (arrival.segment !== logged) {
      logged = arrival.segment;
      console.debug(`arrival: ${logged} at ${Math.round(now())} ms`);
      // What getting in took here, for the next visit's bar and for a
      // report of a slow one.
      if (logged === 'done') {
        rememberArrival(arrival.spent, window);
        console.debug(`arrival: done in ${Math.round(now())} ms (${describeSpent(arrival.spent)})`);
      }
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
        lineView.textContent = shownLine ?? '';
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
