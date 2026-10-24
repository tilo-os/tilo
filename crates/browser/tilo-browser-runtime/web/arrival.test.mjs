import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import {existsSync} from 'node:fs';
import {arrivalLines, arrivalLocale, createArrival, createRotation, doorCopy, failureMessage, FAILURES, fill, inviteLinkTarget, isCoffeeHost, lineParts, refusedByHost, waitListFailure,
  parseMessages, progressIn, SEGMENTS, segmentOf, STEP_NOTES, stoppedInside, visitLine} from './arrival.mjs';

const here = name => new URL(name, import.meta.url);
const locale = name => parseMessages(readFileSync(here(`locales/${name}.ftl`), 'utf8'));
/** A seeded generator, so a failing order can be replayed. */
const seeded = seed => () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
const jokes = Array.from({length: 12}, (_, i) => `joke ${i}`);
const tips = Array.from({length: 4}, (_, i) => `tip ${i}`);
const loading = {arrival: 'load'};
const opening = {arrival: 'open'};

test('a visit opens with a joke, mixes two jokes to a tip and never repeats a line', () => {
  for (let seed = 1; seed < 50; seed++) {
    const rotation = createRotation({jokes, tips}, seeded(seed));
    const shown = [];
    for (let line; (line = rotation.next()) !== null;) shown.push(line.text);
    assert.equal(new Set(shown).size, shown.length, 'no repeats');
    assert.equal(shown.length, jokes.length + tips.length, 'every line once');
    assert.match(shown[0], /^joke/);
    const kinds = shown.slice(0, 12).map(line => line.split(' ')[0]);
    assert.deepEqual(kinds, ['joke', 'joke', 'tip', 'joke', 'joke', 'tip', 'joke', 'joke', 'tip', 'joke', 'joke', 'tip']);
    assert.equal(rotation.next(), null, 'spent');
  }
});

test('lines rotate every 2.5 s from the first second, and the last stays once all are shown', () => {
  // A step that runs long says so instead (below); here none does.
  const arrival = createArrival({lines: {jokes: ['a', 'b'], tips: ['c']}, random: seeded(3), slowMs: {}, stallMs: Infinity});
  assert.equal(arrival.update(opening, 0).line, null);
  assert.equal(arrival.update(opening, 999).line, null, 'nothing before a second');
  assert.equal(arrival.update(opening, 999).bar, false, 'no bar before a second either');
  const first = arrival.update(opening, 1000).line?.text;
  assert.ok(['a', 'b'].includes(first));
  assert.equal(arrival.update(opening, 3400).line.text, first);
  const second = arrival.update(opening, 3500).line.text;
  assert.notEqual(second, first);
  assert.deepEqual(arrival.update(opening, 6000).line, {kind: 'tip', text: 'c'});
  assert.deepEqual(arrival.update(opening, 20_000).line, {kind: 'tip', text: 'c'});
});

test('a load done within the first second never shows a line or bar', () => {
  const arrival = createArrival({lines: {jokes, tips}});
  arrival.update(loading, 100);
  arrival.update({arrival: 'open'}, 400);
  const view = arrival.update({arrival: 'done'}, 900);
  assert.deepEqual({line: view.line, bar: view.bar, note: view.note}, {line: null, bar: false, note: null});
  assert.equal(arrival.update({arrival: 'done'}, 5000).line, null);
});

test('a failure, or anything the page says in its status, shows no joke', () => {
  const arrival = createArrival({lines: {jokes, tips}});
  assert.ok(arrival.update(loading, 1500).line);
  const waiting = arrival.update({arrival: 'join', statusShown: true}, 2000);
  assert.equal(waiting.line, null);
  assert.equal(waiting.bar, true);
  const stopped = arrival.update({arrival: 'stopped', statusShown: true}, 2500);
  assert.deepEqual({line: stopped.line, note: stopped.note, bar: stopped.bar}, {line: null, note: null, bar: false});
});

test('a slow fetch or join says so in plain words instead of a joke', () => {
  const arrival = createArrival({lines: {jokes, tips}, slowNotes: {join: 'Verbindet noch.'}});
  assert.ok(arrival.update(loading, 7999).line);
  const slow = arrival.update(loading, 8000);
  assert.equal(slow.line, null);
  assert.equal(slow.note, 'Still loading. Your network may be slow.');
  assert.ok(arrival.update({arrival: 'load', wasmFetched: true}, 8100).line, 'compiling is not the network');
  arrival.update({arrival: 'join'}, 9000);
  assert.ok(arrival.update({arrival: 'join'}, 14_999).line);
  assert.equal(arrival.update({arrival: 'join'}, 15_000).note, 'Verbindet noch.');
  assert.equal(arrival.update({arrival: 'join', statusShown: true}, 15_100).note, null,
    'what the page says (waiting for the owner) is said instead');
});

test('a first frame that is late says so instead of waiting in silence', () => {
  const arrival = createArrival({lines: {jokes, tips}});
  arrival.update(loading, 0);
  arrival.update({arrival: 'draw'}, 2000);
  assert.ok(arrival.update({arrival: 'draw'}, 11_999).line);
  const slow = arrival.update({arrival: 'draw'}, 12_000);
  assert.equal(slow.line, null);
  assert.equal(slow.note, 'Still starting graphics. Without a graphics card this takes longer.');
  assert.equal(slow.bar, true);
  assert.equal(arrival.update({arrival: 'done'}, 12_100).note, null);
});

// Catches a page left on "Loading Tilo…" (or opening, or starting
// graphics) with nothing to do but wait when a step never finishes.
test('a step that does not move on offers Try again and keeps waiting', () => {
  const arrival = createArrival({lines: {jokes, tips}, slowNotes: {stalled: 'Dauert länger.'}});
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
  const arrival = createArrival({lines: {jokes, tips}});
  arrival.update({arrival: 'load', wasmFetched: true}, 2000);
  assert.equal(arrival.update({arrival: 'load', wasmFetched: true}, 10_000).note, 'Still starting Tilo. This device may be busy.');
  arrival.update({arrival: 'open'}, 11_000);
  assert.equal(arrival.update({arrival: 'open'}, 19_000).note, 'Still opening the workspace.');
});

test('the bar follows the measured phases, creeps within one, and never runs backwards', () => {
  assert.equal(SEGMENTS.fetch.start, 0);
  assert.ok(Math.abs(SEGMENTS.draw.end - 1) < 1e-9);
  assert.ok(Math.abs(SEGMENTS.join.start - 2.55 / 7.15) < 1e-9, 'js, fetch and compile come first');
  for (const elapsed of [0, 500, 5000, 1e9]) {
    const at = progressIn('open', elapsed);
    assert.ok(at >= SEGMENTS.open.start && at < SEGMENTS.open.end, `${elapsed} ms stays inside its phase`);
  }
  assert.ok(progressIn('open', 1000) > progressIn('open', 500));
  const arrival = createArrival();
  const seen = [];
  for (const [state, now] of [[loading, 0], [loading, 1500], [{arrival: 'load', wasmFetched: true}, 2000],
    [{arrival: 'open'}, 2100], [{arrival: 'open'}, 4000], [{arrival: 'draw'}, 4500], [{arrival: 'done'}, 6000]]) {
    seen.push(arrival.update(state, now).progress);
  }
  assert.deepEqual([...seen].sort((a, b) => a - b), seen);
  assert.ok(seen[3] >= SEGMENTS.open.start, 'a skipped join jumps forward');
  assert.equal(seen.at(-1), 1);
  assert.equal(segmentOf('load', false), 'fetch');
  assert.equal(segmentOf('load', true), 'compile');
});

test('with reduced motion the bar steps at each phase instead of creeping', () => {
  const arrival = createArrival({reducedMotion: true});
  assert.equal(arrival.update({arrival: 'open'}, 1000).progress, SEGMENTS.open.start);
  assert.equal(arrival.update({arrival: 'open'}, 3000).progress, SEGMENTS.open.start);
  assert.equal(arrival.update({arrival: 'draw'}, 3100).progress, SEGMENTS.draw.start);
  assert.equal(progressIn('join', 900, {reducedMotion: true}), SEGMENTS.join.start);
});

test('tilo.coffee lines show only on tilo.coffee and its subdomains', () => {
  for (const host of ['tilo.coffee', 'eu1.tilo.coffee', 'TILO.COFFEE.']) assert.ok(isCoffeeHost(host), host);
  for (const host of ['', 'localhost', 'tilo.so', 'notilo.coffee', 'tilo.coffee.example', 'tilo-coffee.example']) {
    assert.ok(!isCoffeeHost(host), host);
  }
  const messages = locale('en-US');
  const elsewhere = arrivalLines(messages, {hostname: 'tilo.example'}).jokes;
  const there = arrivalLines(messages, {hostname: 'eu1.tilo.coffee'}).jokes;
  assert.ok(!elsewhere.includes('Brewing tilo.coffee…'));
  assert.ok(!elsewhere.includes('Fluffing the Lounge cushions…'));
  assert.deepEqual(there.filter(line => !elsewhere.includes(line)), ['Brewing tilo.coffee…', 'Fluffing the Lounge cushions…']);
});

test('desktop tips stay off a full-tab app, and a Mac reads ⌘K', () => {
  const messages = locale('en-US');
  const desktop = arrivalLines(messages, {mode: 'desktop'}).tips;
  assert.ok(desktop.includes('Ctrl+K finds any app'));
  assert.ok(arrivalLines(messages, {mode: 'desktop', mac: true}).tips.includes('⌘K finds any app'));
  assert.ok(!arrivalLines(messages, {mode: 'desktop', mac: true}).tips.includes('Ctrl+K finds any app'));
  const app = arrivalLines(messages, {mode: 'app'}).tips;
  assert.deepEqual(app, ['Everything here is saved as you go', 'Tilo is an operating system. This one runs in your browser.']);
});

test('both locales have jokes and the same tips, in lines short enough to read at a glance', () => {
  // The owner's own lines, kept as written though longer.
  const longer = new Set(['Best viewed in Netscape Navigator 4.0 at 800×600',
    'I’d like to interrupt for a moment. What you are referring to as Li…',
    'Tilo is an operating system. This one runs in your browser.',
    'Tilo ist ein Betriebssystem. Dieses läuft in deinem Browser.']);
  const tipIds = name => [...locale(name).keys()].filter(id => id.includes('tip-')).sort();
  assert.deepEqual(tipIds('de-DE'), tipIds('en-US'));
  for (const name of ['en-US', 'de-DE']) {
    const messages = locale(name);
    const {jokes: shipped, tips: told} = arrivalLines(messages, {hostname: 'tilo.coffee'});
    assert.ok(shipped.length >= 20, `${name} jokes`);
    assert.ok(told.length >= 3, `${name} tips`);
    for (const line of [...shipped, ...told]) {
      if (!longer.has(line)) assert.ok([...line].length <= 44, `${name}: “${line}” is too long`);
    }
    assert.ok(!shipped.some(line => /No account needed/.test(line)));
  }
});

test('the page speaks the desktop\'s language, not the browser\'s', () => {
  assert.equal(arrivalLocale(), 'en-US', 'the desktop starts in English, whatever the browser speaks');
  assert.equal(arrivalLocale('de-AT'), 'de-DE');
  assert.equal(arrivalLocale('de'), 'de-DE');
  assert.equal(arrivalLocale('fr-FR'), 'en-US');
  assert.equal(arrivalLocale(null), 'en-US');
  // A German browser opening the English desktop is greeted, and told of a
  // failure, in English: nothing on the page reads the browser's language.
  assert.ok(!readFileSync(here('arrival.mjs'), 'utf8').includes('navigator?.languages'));
});

test('every step note the entry pages write is one the screen leaves to screen readers', () => {
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

test('a failure says what happened in plain words, never as a raw error', () => {
  const host = Object.assign(new Error('admit: HTTP 502'), {name: 'HostAdmissionError', code: 'unavailable'});
  assert.equal(failureMessage(host, {workspace: 'Lounge'}),
    'Can’t reach Lounge right now. Check your connection, then try again.');
  assert.equal(failureMessage(new TypeError('Failed to fetch'), {loaded: false}),
    'Can’t reach Tilo right now. Check your connection, then try again.');
  assert.equal(failureMessage(new Error('WebSocket connection closed')),
    'Can’t reach the workspace right now. Check your connection, then try again.');
  const refused = Object.assign(new Error('You did not join'), {name: 'AdmissionError', code: 'declined'});
  assert.equal(failureMessage(refused, {workspace: 'Lounge'}), 'You did not join');
  const general = failureMessage(new Error('RuntimeError: unreachable executed at 0x1f'), {workspace: 'Lounge'});
  assert.equal(general, 'Something went wrong opening Lounge. Try again, or come back later.');
  for (const said of [failureMessage(host), general]) assert.doesNotMatch(said, /Failed|HTTP|Error/);
});

test('a failure is said in the person’s language, naming the workspace as the page knows it', () => {
  const german = locale('de-DE');
  const general = new Error('RuntimeError: unreachable executed at 0x1f');
  assert.equal(failureMessage(general, {workspace: 'Kaffeeküche', messages: german}),
    'Beim Öffnen von Kaffeeküche ist etwas schiefgegangen. Versuch es noch einmal oder komm später wieder.');
  assert.equal(failureMessage(general, {messages: german}),
    'Beim Öffnen des Arbeitsbereichs ist etwas schiefgegangen. Versuch es noch einmal oder komm später wieder.');
  assert.equal(failureMessage(new TypeError('Failed to fetch'), {loaded: false, messages: german}),
    'Tilo ist gerade nicht erreichbar. Prüf deine Verbindung und versuch es dann noch einmal.');
  // The English file says what the module says without one.
  for (const error of [general, new TypeError('Failed to fetch')]) {
    for (const options of [{workspace: 'Lounge'}, {}, {admitted: true}, {workspace: 'Lounge', admitted: true}]) {
      assert.equal(failureMessage(error, {...options, messages: locale('en-US')}), failureMessage(error, options));
    }
  }
  assert.equal(locale('de-DE').get('arrival-retry'), 'Noch einmal versuchen');
});

test('a desktop that did not start once the host let the person in says they got in', () => {
  const stopped = new Error('the peer has no replicated room');
  assert.equal(failureMessage(stopped, {workspace: 'Lounge', admitted: true}),
    'You got into Lounge, but its desktop did not start in this tab. Try again.');
  assert.equal(failureMessage(stopped, {workspace: 'Lounge', admitted: true, messages: locale('de-DE')}),
    'Du bist in Lounge, aber der Desktop ist in diesem Tab nicht gestartet. Versuch es noch einmal.');
  // The network failing is said as that, wherever it happened.
  assert.match(failureMessage(new Error('WebSocket connection closed'), {workspace: 'Lounge', admitted: true}), /^Can’t reach Lounge/);
});

test('a person let in whose desktop never drew is told they got in', () => {
  // The renderer gives up on a first frame that never came and marks the
  // arrival stopped before the page hears the mount failed.
  const peer = {room: 'r'};
  assert.equal(stoppedInside(peer, 'stopped'), true);
  assert.equal(stoppedInside(peer, 'open'), true);
  assert.equal(stoppedInside(peer, 'draw'), true);
  assert.equal(stoppedInside(peer, 'done'), false);
  assert.equal(stoppedInside(null, 'stopped'), false);
  const firstFrame = new Error('WebGPU presented no first frame after 60 s on one step, waiting for the first frame to present');
  assert.equal(failureMessage(firstFrame, {workspace: 'Auth', admitted: stoppedInside(peer, 'stopped')}),
    'You got into Auth, but its desktop did not start in this tab. Try again.');
});

test('both locales word every failure, with the workspace’s name and without', () => {
  for (const name of ['en-US', 'de-DE']) {
    const messages = locale(name);
    for (const id of ['arrival-retry', 'arrival-failed-stopped-drawing']) assert.ok(messages.has(id), `${name}: ${id}`);
    for (const kind of Object.keys(FAILURES)) {
      assert.match(messages.get(`arrival-failed-${kind}`) ?? '', /\{ \$workspace \}/, `${name}: ${kind}`);
      const unnamed = messages.get(`arrival-failed-${kind}-unnamed`) ?? '';
      assert.ok(unnamed && !unnamed.includes('{'), `${name}: ${kind}-unnamed`);
    }
  }
  assert.equal(fill('Hallo { $workspace } und {$other}', {workspace: 'Lounge'}), 'Hallo Lounge und {$other}');
});

test('an address the host turns away after a removal is told why and that trying again will not help today', () => {
  const cooling = Object.assign(new Error('join: HTTP 403'), {name: 'HostAdmissionError', code: 'cooling-down'});
  assert.ok(refusedByHost(cooling));
  assert.ok(!refusedByHost(Object.assign(new Error('x'), {name: 'HostAdmissionError', code: 'full'})));
  assert.ok(!refusedByHost(new Error('cooling-down')));
  assert.equal(failureMessage(cooling, {workspace: 'Lounge'}),
    'Lounge is not letting anyone in from your network for now, because someone here was removed from it. '
    + 'Ask someone in it for an invitation, or try again tomorrow.');
  assert.match(failureMessage(cooling), /^This workspace is not letting anyone in/);
  assert.doesNotMatch(failureMessage(cooling), /HTTP|connection/);
});

test('a person the room removed is told so in their language, and trying again does not help', () => {
  const removed = Object.assign(new Error('You were removed from Lounge.'), {name: 'AdmissionError', code: 'removed'});
  const door = Object.assign(new Error('join: HTTP 403'), {name: 'HostAdmissionError', code: 'removed'});
  assert.ok(refusedByHost(door));
  for (const error of [removed, door]) {
    assert.equal(failureMessage(error, {workspace: 'Lounge'}),
      'You were removed from Lounge. To come back, ask someone in it for a new invitation.');
    assert.equal(failureMessage(error, {workspace: 'Lounge', messages: locale('de-DE')}),
      'Du wurdest aus Lounge entfernt. Um zurückzukommen, bitte jemanden darin um eine neue Einladung.');
    assert.equal(failureMessage(error, {messages: locale('en-US')}), failureMessage(error));
  }
});

test('a room taking people by invitation tells a visitor without one that they need one, in their language', () => {
  const door = Object.assign(new Error('join: HTTP 403'), {name: 'HostAdmissionError', code: 'invitation-only'});
  assert.ok(refusedByHost(door));
  assert.equal(failureMessage(door, {workspace: 'Lounge'}),
    'You need an invitation to join Lounge for now. Ask someone in it for an invite link, then open it here.');
  assert.match(failureMessage(door), /^You need an invitation to join this workspace for now\./);
  assert.equal(failureMessage(door, {workspace: 'Lounge', messages: locale('de-DE')}),
    'Für Lounge brauchst du im Moment eine Einladung. Bitte jemanden darin um einen Einladungslink und öffne ihn hier.');
  assert.equal(failureMessage(door, {messages: locale('en-US')}), failureMessage(door));
});

test('a pasted invite link opens as itself; anything else is not one', () => {
  const base = 'https://tilo.coffee/';
  const link = 'https://tilo.coffee/#/desktop/room/abc?invite=XYZ';
  assert.equal(inviteLinkTarget(`  ${link}\n`, base), link);
  assert.equal(inviteLinkTarget('https://tilo.coffee/?invite=XYZ', base), 'https://tilo.coffee/?invite=XYZ');
  for (const text of ['', 'hello', 'https://tilo.coffee/', 'https://tilo.coffee/#/desktop/room/abc', 'javascript:alert(1)?invite=1', null]) {
    assert.equal(inviteLinkTarget(text, base), null, String(text));
  }
});

test('a page that could not become a device of its own says other tabs hold every one', () => {
  const busy = Object.assign(new Error('Every device key is held by another tab'), {name: 'CustodyError', code: 'custody-busy'});
  assert.equal(failureMessage(busy, {workspace: 'Lounge'}),
    'Every device this browser keeps for Lounge is open in another tab. Close one of those tabs, then try again.');
  assert.equal(failureMessage(busy),
    'Every device this browser keeps for the workspace is open in another tab. Close one of those tabs, then try again.');
});

test('a tip reads after its label, a joke bare', () => {
  const rotation = createRotation({jokes: ['Herding agents…'], tips: ['Ctrl+K finds any app']});
  const joke = rotation.next();
  const tip = rotation.next();
  assert.deepEqual(lineParts(joke, 'Tip'), {label: null, text: 'Herding agents…'});
  assert.deepEqual(lineParts(tip, 'Tip'), {label: 'Tip', text: 'Ctrl+K finds any app'});
  assert.equal(locale('en-US').get('arrival-label-tip'), 'Tip');
  assert.equal(locale('de-DE').get('arrival-label-tip'), 'Tipp');
  // The label is no tip of its own.
  assert.ok(!arrivalLines(locale('de-DE')).tips.includes('Tipp'));
});

test('a watching page promises a join only where the door gives one', () => {
  const open = {admission: 'open', joins: true};
  assert.deepEqual(visitLine({entered: 'watching', presence: open}), {line: 'Click or type to join', join: 'gesture'});
  // A door that did not answer: the page offers what it always has.
  assert.equal(visitLine({entered: 'watching'}).join, 'gesture');
  const invited = visitLine({entered: 'watching', presence: {admission: 'invite', joins: true}, workspace: 'Lounge'});
  assert.equal(invited.join, 'none');
  assert.match(invited.line, /^You’re watching live\. You need an invitation to join Lounge for now\./);
  for (const entered of ['watching', 'seatless']) {
    const closed = visitLine({entered, because: 'unavailable', presence: {admission: 'open', joins: false}});
    assert.equal(closed.join, 'seat', 'it asks again, and joins (or offers to) once the room opens');
    assert.doesNotMatch(closed.line, /in a moment|Click/);
    assert.match(closed.line, /closed for now/);
  }
  assert.match(visitLine({entered: 'seatless', because: 'full', presence: open}).line, /when a seat frees up/);
  // A member of an invite-only room without a seat still joins once one frees.
  assert.equal(visitLine({entered: 'seatless', because: 'full', presence: {admission: 'invite', joins: true}}).join, 'seat');
});

test('the door apologises and asks for an email where there is a wait list, else names the workspace', () => {
  assert.deepEqual(doorCopy({workspace: 'Lounge', waitList: true}), {
    heading: 'Sorry, Tilo is invite-only right now',
    body: 'It’s an early alpha. Leave your email and we’ll send you an invite when there’s room.',
  });
  assert.deepEqual(doorCopy({workspace: 'Lounge'}), {heading: 'Lounge is invite-only for now',
    body: 'Ask someone in it for an invite link, then paste it here.'});
  assert.equal(doorCopy().heading, 'This workspace is invite-only for now');
  const german = doorCopy({waitList: true, messages: locale('de-DE')});
  assert.match(german.heading, /Einladung/);
  assert.match(german.body, /Warteliste|E-Mail/);
  assert.equal(waitListFailure('invalid-email'), 'That doesn’t look like an email address.');
  assert.match(waitListFailure('wait-list-full'), /full for now/);
  assert.match(waitListFailure('too-many'), /Try again later/);
  assert.equal(waitListFailure('unavailable'), waitListFailure(undefined));
  assert.match(waitListFailure('invalid-email', locale('de-DE')), /E-Mail-Adresse/);
});

test('both locales word the whole door, its wait list and signing in', () => {
  const en = locale('en-US');
  const de = locale('de-DE');
  const door = [...en.keys()].filter(id => /^arrival-(door|wait|sign-in|invite)/.test(id));
  assert.ok(door.length >= 25, `${door.length} door messages`);
  for (const id of door) assert.ok(de.has(id), `de-DE: ${id}`);
  for (const [id, arg] of [['arrival-wait-done', 'email'], ['arrival-wait-removal', 'contact'], ['arrival-sign-in-not-member', 'workspace']]) {
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
