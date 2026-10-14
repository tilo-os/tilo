/**
 * The arrival screen's presentation (index.html, `#arrival`): while the
 * desktop is on its way, a thin progress bar moved by the real steps of
 * getting in, and above it one rotating line, a joke or a tip, from the
 * locale files beside this module (`locales/<locale>.ftl`).
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
  const pools = {joke: shuffled(jokes), tip: shuffled(tips)};
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

/** Said in place of the line when a step that waits on the network runs
 * long; the locale file may word them. */
export const SLOW_NOTES = {
  fetch: ['arrival-slow-loading', 'Still loading. Your network may be slow.'],
  join: ['arrival-slow-connecting', 'Still connecting. Your network may be slow.'],
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
  delayMs = 1000, periodMs = 2500, slowMs = {fetch: 8000, join: 6000}, slowNotes = {}} = {}) {
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

/** The locale whose lines this browser gets: German for German, else English. */
export function arrivalLocale(languages = globalThis.navigator?.languages ?? []) {
  return languages.find(language => /^(de|en)\b/i.test(language))?.toLowerCase().startsWith('de') ? 'de-DE' : 'en-US';
}

/**
 * Run the arrival screen on the page: reads `data-arrival` and `#status`,
 * writes the line (`#arrival-line`), the bar (`#arrival-bar`) and a slow
 * network's note (`#arrival-note`). Stops once the desktop is shown.
 */
export function startArrival({mode = 'desktop', document = globalThis.document, window = globalThis} = {}) {
  const root = document.documentElement;
  const lineView = document.getElementById('arrival-line');
  const barView = document.getElementById('arrival-bar');
  const noteView = document.getElementById('arrival-note');
  const status = document.getElementById('status');
  if (!lineView || !barView || !noteView || !status) return null;
  const reducedMotion = Boolean(window.matchMedia?.('(prefers-reduced-motion: reduce)').matches);
  const locale = arrivalLocale(window.navigator?.languages);
  const mac = /Mac/.test(window.navigator?.platform ?? '');
  const slowNotes = {};
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
  void window.fetch(new URL(`locales/${locale}.ftl`, import.meta.url))
    .then(response => (response.ok ? response.text() : ''))
    .then(text => {
      const messages = parseMessages(text);
      arrival.setLines(arrivalLines(messages, {mode, mac, hostname: window.location?.hostname}));
      for (const [segment, [id]] of Object.entries(SLOW_NOTES)) {
        if (messages.has(id)) slowNotes[segment] = messages.get(id);
      }
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
      if (reducedMotion || !lineView.textContent || view.line === null) {
        lineView.textContent = view.line ?? '';
        lineView.classList.toggle('shown', view.line !== null);
      } else {
        // A crossfade: out, the new words, back in.
        lineView.classList.remove('shown');
        swap = setTimeout(() => { lineView.textContent = shownLine ?? ''; lineView.classList.toggle('shown', shownLine !== null); }, 240);
      }
    }
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
