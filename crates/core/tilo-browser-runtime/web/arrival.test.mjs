import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {existsSync} from 'node:fs';
import {arrivalLocale, createArrival, describeSpent, refusalCopy, fill,
  FIRST_VISIT_SECONDS, entryLinkTarget, measuredPlan, measuredSeconds, plannedSegments, refusedByHost,
  rememberArrival, rememberedArrival, waitListFailure, MEASURED_KEY, parseMessages, progressIn, SEGMENTS, segmentOf,
  STEP_NOTES, stoppedInside, visitorLocale} from './arrival.mjs';

const here = name => new URL(name, import.meta.url);
const locale = name => parseMessages(readFileSync(here(`locales/${name}.ftl`), 'utf8'));
const loading = {arrival: 'load', stepNote: 'Loading Tilo…'};
const opening = {arrival: 'open', stepNote: 'Opening the workspace…'};
const joining = {arrival: 'join', stepNote: 'Joining…'};

test('the line says the step getting in is on, from the first second, and nothing else', () => {
  // A step that runs long says so instead (below); here none does.
  const arrival = createArrival({slowMs: {}, stallMs: Infinity});
  assert.equal(arrival.update(loading, 0).line, null);
  assert.equal(arrival.update(loading, 999).line, null, 'nothing before a second');
  assert.equal(arrival.update(loading, 999).bar, false, 'no bar before a second either');
  assert.equal(arrival.update(loading, 1000).line, 'Loading Tilo…');
  assert.equal(arrival.update(joining, 1500).line, 'Joining…');
  assert.equal(arrival.update(opening, 20_000).line, 'Opening the workspace…');
  assert.equal(arrival.update({arrival: 'open'}, 20_100).line, null, 'a page that says no step shows no line');
});
test('a load done within the first second never shows a line or bar', () => {
  const arrival = createArrival();
  arrival.update(loading, 100);
  arrival.update({arrival: 'open'}, 400);
  const view = arrival.update({arrival: 'done'}, 900);
  assert.deepEqual({line: view.line, bar: view.bar, note: view.note}, {line: null, bar: false, note: null});
  assert.equal(arrival.update({arrival: 'done'}, 5000).line, null);
});

test('a failure, or anything the page says in its status, shows no step', () => {
  const arrival = createArrival();
  assert.ok(arrival.update(loading, 1500).line);
  const waiting = arrival.update({...joining, statusShown: true}, 2000);
  assert.equal(waiting.line, null);
  assert.equal(waiting.bar, true);
  const stopped = arrival.update({arrival: 'stopped', statusShown: true}, 2500);
  assert.deepEqual({line: stopped.line, note: stopped.note, bar: stopped.bar}, {line: null, note: null, bar: false});
});

test('a slow fetch or join says so in plain words instead of the step', () => {
  const arrival = createArrival({slowNotes: {join: 'Verbindet noch.'}});
  assert.ok(arrival.update(loading, 7999).line);
  const slow = arrival.update(loading, 8000);
  assert.equal(slow.line, null);
  assert.equal(slow.note, 'Still loading Tilo.');
  assert.ok(arrival.update({...loading, wasmFetched: true}, 8100).line, 'compiling is not the network');
  arrival.update(joining, 9000);
  assert.ok(arrival.update(joining, 14_999).line);
  assert.equal(arrival.update(joining, 15_000).note, 'Verbindet noch.');
  assert.equal(arrival.update({arrival: 'join', statusShown: true}, 15_100).note, null,
    'what the page says (waiting for the owner) is said instead');
});

test('a first frame that is late says so instead of waiting in silence', () => {
  const arrival = createArrival();
  arrival.update(loading, 0);
  const drawing = {arrival: 'draw', stepNote: 'Starting graphics…'};
  arrival.update(drawing, 2000);
  assert.ok(arrival.update(drawing, 11_999).line);
  const slow = arrival.update(drawing, 12_000);
  assert.equal(slow.line, null);
  assert.equal(slow.note, 'Still starting graphics. Without a graphics card this takes longer.');
  assert.equal(slow.bar, true);
  assert.equal(arrival.update({arrival: 'done'}, 12_100).note, null);
});

// Catches a page left on "Loading Tilo…" (or opening, or starting
// graphics) with nothing to do but wait when a step never finishes.
test('a step that does not move on offers Try again and keeps waiting', () => {
  const arrival = createArrival({slowNotes: {stalled: 'Dauert länger.'}});
  let at = 0;
  for (const step of [loading, {arrival: 'load', wasmFetched: true}, {arrival: 'join'}, {arrival: 'open'}]) {
    assert.equal(arrival.update(step, at).stalled, false, `${step.arrival} starts unstalled`);
    assert.equal(arrival.update(step, at + 29_999).stalled, false);
    const stalled = arrival.update(step, at + 30_000);
    assert.equal(stalled.stalled, true, step.arrival);
    assert.equal(stalled.note, 'Dauert länger.');
    assert.equal(stalled.bar, true, 'the bar stays: it is still waiting');
    at += 40_000;
  }
  // Waiting for a person (to be let in) never stalls.
  arrival.update({arrival: 'join', statusShown: true}, 200_000);
  assert.equal(arrival.update({arrival: 'join', statusShown: true}, 900_000).stalled, false);
  // Graphics stall when the runtime says so, not by the clock: its steps
  // (device, pipelines, first frame) each get their own time.
  arrival.update({arrival: 'draw'}, 1_000_000);
  assert.equal(arrival.update({arrival: 'draw'}, 1_500_000).stalled, false);
  assert.equal(arrival.update({arrival: 'draw', gpuStalled: true}, 1_500_001).stalled, true);
  assert.equal(arrival.update({arrival: 'done', gpuStalled: true}, 1_500_002).stalled, false);
});

test('a slow start or opening says so too', () => {
  const arrival = createArrival();
  arrival.update({arrival: 'load', wasmFetched: true}, 2000);
  assert.equal(arrival.update({arrival: 'load', wasmFetched: true}, 10_000).note, 'Still starting Tilo.');
  arrival.update({arrival: 'open'}, 11_000);
  assert.equal(arrival.update({arrival: 'open'}, 19_000).note, 'Still opening the workspace.');
});

test('the bar is sized by the steps and creeps within one, and never runs backwards', () => {
  assert.equal(SEGMENTS.fetch.start, 0);
  assert.ok(Math.abs(SEGMENTS.draw.end - 1) < 1e-9);
  const total = Object.values(FIRST_VISIT_SECONDS).reduce((sum, seconds) => sum + seconds, 0);
  assert.ok(Math.abs(SEGMENTS.join.start - (FIRST_VISIT_SECONDS.fetch + FIRST_VISIT_SECONDS.compile) / total) < 1e-9,
    'fetch and compile come first');
  for (const elapsed of [0, 500, 5000, 1e9]) {
    const at = progressIn('open', elapsed);
    assert.ok(at >= SEGMENTS.open.start && at < SEGMENTS.open.end, `${elapsed} ms stays inside its phase`);
  }
  assert.ok(progressIn('open', 1000) > progressIn('open', 500));
  const arrival = createArrival();
  const seen = [];
  const plans = [];
  for (const [state, now] of [[loading, 0], [loading, 1500], [{arrival: 'load', wasmFetched: true}, 2000],
    [{arrival: 'open'}, 2100], [{arrival: 'open'}, 4000], [{arrival: 'draw'}, 4500], [{arrival: 'done'}, 6000]]) {
    seen.push(arrival.update(state, now).progress);
    plans.push(plannedSegments(arrival.seconds));
  }
  assert.deepEqual([...seen].sort((a, b) => a - b), seen);
  // A step that did not happen leaves no time behind it: the bar stands
  // where the time spent so far belongs in the plan it made as it went.
  assert.ok(seen[3] >= plans[3].open.start - 1e-9, 'a skipped join jumps forward');
  assert.equal(seen.at(-1), 1);
  assert.equal(segmentOf('load', false), 'fetch');
  assert.equal(segmentOf('load', true), 'compile');
});

test('the bar is planned from what getting in took here, not from a guess', () => {
  // A device that measured itself: fetch and compile took longer, the rest
  // shorter, so each step's share of the bar follows.
  const measured = {fetch: 400, compile: 1200, join: 90, open: 110, draw: 2600};
  const plan = measuredPlan(measured);
  const arrival = createArrival({measured});
  assert.deepEqual(arrival.seconds, {...FIRST_VISIT_SECONDS, ...plan});
  const done = plannedSegments(plan);
  assert.ok(done.draw.end - done.draw.start > SEGMENTS.draw.end - SEGMENTS.draw.start,
    'a slower draw is given more of the bar');
  // Within the step the bar creeps toward its end at the step's own pace: a
  // 2.6 s draw still moves at 2 s, where the first visit's 1.6 s guess had
  // already flattened out.
  const guessed = progressIn('draw', 2000);
  const planned = progressIn('draw', 2000, {seconds: plan});
  assert.ok(planned > done.draw.start + (done.draw.end - done.draw.start) * 0.4, 'measured draw keeps moving');
  for (const elapsed of [0, 500, 1500, 2600, 10_000]) {
    const at = progressIn('draw', elapsed, {seconds: plan});
    assert.ok(at >= done.draw.start && at < done.draw.end, `${elapsed} ms stays inside its step`);
  }
  // A step's real time replaces its estimate as it completes.
  const slowJoin = createArrival({measured});
  slowJoin.update({arrival: 'load'}, 0);
  slowJoin.update({arrival: 'load', wasmFetched: true}, 400);
  slowJoin.update({arrival: 'join'}, 1600);
  assert.deepEqual(slowJoin.spent, {fetch: 400, compile: 1200});
  slowJoin.update({arrival: 'open'}, 6000);
  assert.equal(slowJoin.spent.join, 4400, 'the join took far longer than measured');
  assert.equal(slowJoin.seconds.join, 4.4, 'and the plan says so now, in seconds');
});

test('a visit remembers what it took, and reads it back for the next bar', () => {
  const kept = new Map();
  const window = {localStorage: {getItem: key => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, value)}};
  assert.deepEqual(rememberedArrival(window), {});
  rememberArrival({fetch: 210, compile: 70.4, join: NaN, open: 240, draw: 'long'}, window);
  assert.deepEqual(rememberedArrival(window), {fetch: 210, compile: 70.4, open: 240});
  assert.deepEqual(measuredSeconds({fetch: 0, compile: -1, join: Infinity}), {});
  assert.equal(describeSpent({fetch: 210, join: 110.6}), 'fetch 210 ms · join 111 ms');
  // A page that keeps nothing (private mode, no storage) still shows a bar.
  const refusing = {localStorage: {getItem() { throw new Error('denied'); }, setItem() { throw new Error('denied'); }}};
  assert.deepEqual(rememberedArrival(refusing), {});
  rememberArrival({fetch: 1}, refusing);
  assert.equal(kept.has(MEASURED_KEY), true);
});

test('with reduced motion the bar steps at each phase instead of creeping', () => {
  const arrival = createArrival({reducedMotion: true});
  assert.equal(arrival.update({arrival: 'open'}, 1000).progress, SEGMENTS.open.start);
  assert.equal(arrival.update({arrival: 'open'}, 3000).progress, SEGMENTS.open.start);
  // The opening's two seconds replace its estimate, so the plan the draw
  // starts at is the one that now holds what the step really took.
  // The opening ran from the page's start (its first segment), so it says it took 3.1 s.
  const next = plannedSegments({...FIRST_VISIT_SECONDS, open: 3.1});
  assert.equal(arrival.update({arrival: 'draw'}, 3100).progress, next.draw.start);
  assert.equal(progressIn('join', 900, {reducedMotion: true}), SEGMENTS.join.start);
});

test('the arrival screen speaks the desktop\'s language, the refusal the visitor\'s', () => {
  assert.equal(arrivalLocale(), 'en-US', 'the desktop starts in English, whatever the browser speaks');
  assert.equal(arrivalLocale('de-AT'), 'de-DE');
  assert.equal(arrivalLocale('de'), 'de-DE');
  assert.equal(arrivalLocale('fr-FR'), 'en-US');
  assert.equal(arrivalLocale(null), 'en-US');
  // A German browser opening the English desktop is greeted, and told of a
  // failure, in English. One the entry turns away has no desktop behind the
  // refusal, so the refusal speaks the visitor's language.
  assert.equal(visitorLocale({navigator: {languages: ['de-DE', 'en-US']}}), 'de-DE');
  assert.equal(visitorLocale({navigator: {languages: ['fr-FR', 'de-AT']}}), 'de-DE',
    'the first of its languages this page has a catalogue for, not the browser\'s first');
  assert.equal(visitorLocale({navigator: {languages: ['fr-FR'], language: 'fr-FR'}}), 'en-US');
  assert.equal(visitorLocale({navigator: {language: 'de'}}), 'de-DE', 'a browser that names only its language');
  assert.equal(visitorLocale({}), 'en-US');
});

test('every step note the entry pages write is one the line shows', () => {
  const sources = ['entry.mjs', 'entry-page.mjs', 'index.html', 'app/index.html']
    .map(name => readFileSync(here(name), 'utf8')).join('\n');
  for (const note of STEP_NOTES) assert.ok(sources.includes(`${note}`), `${note} is no longer written`);
  // What a person must read stays on screen.
  for (const shown of ['Waiting for someone in the workspace to let you in…', 'Pairing code']) {
    assert.ok(![...STEP_NOTES].some(note => note.startsWith(shown)));
  }
});

test('every locale the page can pick has its file beside the module', () => {
  const picked = new Set([undefined, 'de-DE', 'en-US', 'fr'].map(arrivalLocale));
  for (const name of picked) assert.ok(existsSync(here(`locales/${name}.ftl`)), name);
});

test('a person let in whose desktop never drew is told they got in', () => {
  // The renderer gives up on a first frame that never came and marks the
  // arrival stopped before the page hears the mount failed.
  const peer = {workspace: 'r'};
  assert.equal(stoppedInside(peer, 'stopped'), true);
  assert.equal(stoppedInside(peer, 'open'), true);
  assert.equal(stoppedInside(peer, 'draw'), true);
  assert.equal(stoppedInside(peer, 'done'), false);
  assert.equal(stoppedInside(null, 'stopped'), false);
});

test('a placeholder the arguments lack stays as written', () => {
  assert.equal(fill('Hallo { $workspace } und {$other}', {workspace: 'Lounge'}), 'Hallo Lounge und {$other}');
  assert.equal(locale('de-DE').get('arrival-retry'), 'Noch einmal versuchen');
});

test('an entry turning an address away after a removal is the host’s own answer', () => {
  const cooling = Object.assign(new Error('join: HTTP 403'), {name: 'HostAdmissionError', code: 'cooling-down'});
  assert.ok(refusedByHost(cooling));
  assert.ok(!refusedByHost(Object.assign(new Error('x'), {name: 'HostAdmissionError', code: 'full'})));
  assert.ok(!refusedByHost(new Error('cooling-down')));
});

test('a removal the entry says is the host’s own answer', () => {
  const removed = Object.assign(new Error('You were removed from Lounge.'), {name: 'AdmissionError', code: 'removed'});
  const entry = Object.assign(new Error('join: HTTP 403'), {name: 'HostAdmissionError', code: 'removed'});
  assert.ok(refusedByHost(entry));
  assert.ok(!refusedByHost(removed), 'the page’s own reading of a removal is no entry’s answer');
});

test('an entry taking people by invitation is the host’s own answer', () => {
  const entry = Object.assign(new Error('join: HTTP 403'), {name: 'HostAdmissionError', code: 'invitation-only'});
  assert.ok(refusedByHost(entry));
});

test('a pasted invite or pairing link opens as itself; anything else is not one', () => {
  const base = 'https://tilo.sh/';
  const link = 'https://tilo.sh/#/desktop/workspace/abc?invite=XYZ';
  assert.equal(entryLinkTarget(`  ${link}\n`, base), link);
  assert.equal(entryLinkTarget('https://tilo.sh/?invite=XYZ', base), 'https://tilo.sh/?invite=XYZ');
  // The link "Pair a new device" makes: all a person with no identity on a
  // new device holds at the entry.
  const pairing = `https://tilo.sh/#/@workspaces.example/w/abc?pair=${'aa'.repeat(32)}${'0f'.repeat(16)}`;
  assert.equal(entryLinkTarget(pairing, base), pairing);
  assert.equal(entryLinkTarget(`${pairing} `, 'https://tilo.sh/#/'), pairing);
  // The short link the invite sheet hands over: the invitation its workspace
  // keeps at its host, named by the secret it carries.
  const short = `https://tilo.sh/#/@node.example/w/abc?invite=${'k'.repeat(43)}`;
  assert.equal(entryLinkTarget(short, base), short);
  for (const text of ['', 'hello', 'https://tilo.sh/', 'https://tilo.sh/#/desktop/workspace/abc', 'javascript:alert(1)?invite=1',
    'javascript:alert(1)?pair=1', `https://tilo.sh/#${'k'.repeat(43)}`, `https://tilo.sh/elsewhere#${'k'.repeat(43)}`, null]) {
    assert.equal(entryLinkTarget(text, base), null, String(text));
  }
});

test('the entry says what Tilo is and asks for an email where there is a wait list, else names the workspace', () => {
  assert.deepEqual(refusalCopy({workspace: 'Lounge', waitList: true}), {
    heading: 'One workspace for you, your team and your AI agents',
    body: 'Notes, tasks, files and chat in one place, on all your devices. Claude Code and other agents work in it beside you, and you see everything they do. Tilo is invite-only during its early alpha: leave your email and we’ll send you an invite.',
  });
  assert.deepEqual(refusalCopy({workspace: 'Lounge'}), {heading: 'Lounge is invite-only for now',
    body: 'Ask someone in it for an invite link, then paste it here. If you are in it on another device, paste the link “Pair a new device” makes there.'});
  assert.equal(refusalCopy().heading, 'This workspace is invite-only for now');
  const german = refusalCopy({waitList: true, messages: locale('de-DE')});
  assert.match(german.heading, /KI-Agenten/);
  assert.match(german.body, /Einladung/);
  assert.equal(waitListFailure('invalid-email'), 'That doesn’t look like an email address.');
  assert.match(waitListFailure('wait-list-full'), /full for now/);
  assert.match(waitListFailure('too-many'), /Try again later/);
  assert.equal(waitListFailure('unavailable'), waitListFailure(undefined));
  assert.match(waitListFailure('invalid-email', locale('de-DE')), /E-Mail-Adresse/);
});

test('both locales word the whole entry, its wait list and signing in', () => {
  const en = locale('en-US');
  const de = locale('de-DE');
  const entry = [...en.keys()].filter(id => /^arrival-(entry|wait|sign-in|invite)/.test(id));
  assert.ok(entry.length >= 25, `${entry.length} entry messages`);
  for (const id of entry) assert.ok(de.has(id), `de-DE: ${id}`);
  for (const [id, arg] of [['arrival-wait-inbox', 'email'], ['arrival-wait-listed', 'email'], ['arrival-wait-contact', 'contact']]) {
    for (const messages of [en, de]) assert.ok(messages.get(id).includes(`{ $${arg} }`), id);
  }
});

/** stall.inline.js run against a page whose modules have not run: its
 * root, status line and the listeners and timer it set. */
function stallPage() {
  const listeners = {};
  const attributes = new Set();
  const root = {dataset: {arrival: 'load'}, setAttribute: name => attributes.add(name)};
  const status = {textContent: 'Loading Tilo…'};
  let timer;
  const page = {
    document: {documentElement: root, getElementById: id => (id === 'status' ? status : null)},
    addEventListener: (type, listener) => { (listeners[type] ??= []).push(listener); },
    setTimeout: (fn, ms) => { timer = {fn, ms}; },
    location: {reloads: 0, reload() { this.reloads++; }},
  };
  vm.runInNewContext(readFileSync(here('stall.inline.js'), 'utf8'), page);
  const fire = (type, event) => { for (const listener of listeners[type] ?? []) listener(event); };
  return {root, status, attributes, timer: () => timer, fire, location: page.location};
}

// Catches a page whose module failed to load sitting on "Loading Tilo…"
// for good, with no script left to say so or to try again.
test('a module that fails to load stops the page with Try again, which reloads', () => {
  const page = stallPage();
  page.fire('error', {target: {tagName: 'SCRIPT'}});
  assert.equal(page.root.dataset.arrival, 'stopped');
  assert.equal(page.root.dataset.entry, 'failed');
  assert.match(page.status.textContent, /could not load/);
  page.fire('click', {target: {id: 'arrival-retry'}});
  assert.equal(page.location.reloads, 1);
  // Once the arrival screen runs, a failing script is its page's to handle.
  const running = stallPage();
  running.root.dataset.arrivalProgress = '0.100';
  running.fire('error', {target: {tagName: 'SCRIPT'}});
  assert.equal(running.root.dataset.arrival, 'load');
  const image = stallPage();
  image.fire('error', {target: {tagName: 'IMG'}});
  assert.equal(image.root.dataset.arrival, 'load');
});

test('modules still not running after a while offer Try again while they keep loading', () => {
  const slow = stallPage();
  assert.ok(slow.timer().ms <= 30_000);
  slow.timer().fn();
  assert.ok(slow.attributes.has('data-arrival-stalled'));
  assert.equal(slow.root.dataset.arrival, 'load', 'still loading');
  const started = stallPage();
  started.root.dataset.arrivalProgress = '0.200';
  started.timer().fn();
  assert.ok(!started.attributes.has('data-arrival-stalled'));
});

test('every entry page inlines the stall fallback verbatim after the scrubber, and shows Try again when stalled', () => {
  const fallback = readFileSync(here('stall.inline.js'), 'utf8');
  for (const page of ['index.html', 'app/index.html']) {
    const html = readFileSync(here(page), 'utf8');
    assert.ok(html.includes(`</script>\n<script>\n${fallback}</script>`), `${page} must inline stall.inline.js unchanged`);
    assert.match(html, /\[data-arrival-stalled\][^{]*#arrival-retry \{ display: block; \}/, page);
  }
});
