import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import {existsSync} from 'node:fs';
import {arrivalLines, arrivalLocale, createArrival, createRotation, failureMessage, isCoffeeHost, lineParts, refusedByHost,
  parseMessages, progressIn, SEGMENTS, segmentOf, STEP_NOTES} from './arrival.mjs';

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
  const arrival = createArrival({lines: {jokes: ['a', 'b'], tips: ['c']}, random: seeded(3)});
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

test('a German browser gets German lines, any other English', () => {
  assert.equal(arrivalLocale(['de-AT', 'en']), 'de-DE');
  assert.equal(arrivalLocale(['de']), 'de-DE');
  assert.equal(arrivalLocale(['fr-FR', 'de-DE']), 'de-DE');
  assert.equal(arrivalLocale(['en-GB', 'de']), 'en-US');
  assert.equal(arrivalLocale([]), 'en-US');
});

test('every step note the entry pages write is one the screen leaves to screen readers', () => {
  const sources = ['entry.mjs', 'entry-page.mjs', 'index.html', 'app/index.html']
    .map(name => readFileSync(here(name), 'utf8')).join('\n');
  for (const note of STEP_NOTES) assert.ok(sources.includes(`${note}`), `${note} is no longer written`);
  // What a person must read stays on screen.
  for (const shown of ['Waiting for the workspace owner to let you in…', 'Pairing code']) {
    assert.ok(![...STEP_NOTES].some(note => note.startsWith(shown)));
  }
});

test('every locale the page can pick has its file beside the module', () => {
  const picked = new Set([[], ['de-DE'], ['en-US'], ['fr']].map(arrivalLocale));
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
