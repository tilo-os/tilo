import test from 'node:test';
import assert from 'node:assert/strict';
import {BACKOFF, backoffDelay, countdownText, createReconnect, domReconnectView, reconnectDetails, reconnectReason, redact} from './reconnect.mjs';

/** A clock, timers, a window and a document to drive the wait by hand. */
function harness({hidden = false, online = true} = {}) {
  let now = 1_000_000;
  let next = 0;
  const timers = new Map();
  const schedule = (run, ms) => { timers.set(++next, {at: now + ms, run}); return next; };
  const cancel = id => { timers.delete(id); };
  const advance = ms => {
    const end = now + ms;
    for (;;) {
      const due = [...timers].filter(([, timer]) => timer.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      timers.delete(due[0]);
      now = due[1].at;
      due[1].run();
    }
    now = end;
  };
  const target = () => {
    const listeners = {};
    return {
      addEventListener: (type, listener) => { (listeners[type] ??= new Set()).add(listener); },
      removeEventListener: (type, listener) => { listeners[type]?.delete(listener); },
      dispatch: type => { for (const listener of listeners[type] ?? []) listener(); },
    };
  };
  const window = {...target(), navigator: {onLine: online}};
  const document = {...target(), visibilityState: hidden ? 'hidden' : 'visible'};
  const view = {shown: [], hidden: 0, show(state) { this.shown.push(state); }, hide() { this.hidden += 1; }};
  const reconnect = createReconnect({view, window, document, now: () => now, schedule, cancel, random: () => 0.5});
  return {reconnect, view, window, document, advance, now: () => now};
}

/** Whether `promise` settled by now (microtasks run first). */
async function settled(promise) {
  let done = false;
  promise.then(() => { done = true; }, () => { done = true; });
  await new Promise(resolve => setImmediate(resolve));
  return done;
}

test('the waits grow twice as long from 2 s, up to 5 min, each within a fifth either way', () => {
  const middle = Array.from({length: 11}, (_, attempt) => backoffDelay(attempt, () => 0.5));
  assert.deepEqual(middle, [2000, 4000, 8000, 16_000, 32_000, 64_000, 128_000, 256_000, 300_000, 300_000, 300_000]);
  for (let attempt = 0; attempt < 14; attempt += 1) {
    const base = Math.min(BACKOFF.capMs, BACKOFF.firstMs * 2 ** attempt);
    for (const random of [0, 0.01, 0.37, 0.5, 0.93, 0.999999]) {
      const delay = backoffDelay(attempt, () => random);
      assert.ok(delay >= Math.floor(base * 0.8) && delay <= Math.min(BACKOFF.capMs, Math.ceil(base * 1.2)), `${attempt} ${random}: ${delay}`);
    }
  }
  assert.equal(backoffDelay(0, () => 0), 1600);
  assert.equal(backoffDelay(0, () => 1), 2400);
  assert.equal(backoffDelay(40, () => 1), BACKOFF.capMs, 'never past the cap, jitter or not');
});

test('a wait counts down, then the page tries, and says so', async () => {
  const {reconnect, view, advance} = harness();
  const waiting = reconnect.wait({reason: 'unreachable', attempt: 2, origin: 'https://lounge.example/a?b'});
  assert.equal(view.shown.at(-1).waiting, 'counting');
  assert.equal(view.shown.at(-1).remainingMs, 8000);
  advance(3000);
  assert.equal(view.shown.at(-1).remainingMs, 5000);
  assert.equal(await settled(waiting), false);
  advance(5000);
  assert.equal(await settled(waiting), true);
  assert.equal(view.shown.at(-1).waiting, 'trying');
  reconnect.done();
  assert.equal(view.hidden, 1);
});

test('Try now tries at once, and the next wait counts down anew', async () => {
  const {reconnect, view, advance} = harness();
  const first = reconnect.wait({reason: 'refused', attempt: 5});
  advance(1000);
  reconnect.tryNow();
  assert.equal(await settled(first), true);
  assert.equal(view.shown.at(-1).waiting, 'trying');
  reconnect.tryNow();
  const second = reconnect.wait({reason: 'refused', attempt: 6});
  assert.equal(view.shown.at(-1).remainingMs, 128_000, 'from the next failure, its whole wait');
  assert.equal(await settled(second), false);
});

test('a hidden tab tries nothing, and tries at once when shown after its time', async () => {
  const {reconnect, document, advance} = harness({hidden: true});
  const waiting = reconnect.wait({reason: 'unreachable', attempt: 0});
  advance(60_000);
  assert.equal(await settled(waiting), false, 'paused while hidden');
  document.visibilityState = 'visible';
  document.dispatch('visibilitychange');
  assert.equal(await settled(waiting), true);
});

test('a tab shown again before its time keeps counting down', async () => {
  const {reconnect, document, view, advance} = harness();
  const waiting = reconnect.wait({reason: 'unreachable', attempt: 3});
  document.visibilityState = 'hidden';
  document.dispatch('visibilitychange');
  advance(5000);
  document.visibilityState = 'visible';
  document.dispatch('visibilitychange');
  assert.equal(await settled(waiting), false);
  assert.equal(view.shown.at(-1).remainingMs, 11_000);
  advance(11_000);
  assert.equal(await settled(waiting), true);
});

test('offline, the page waits for the network and tries the moment it is back', async () => {
  const {reconnect, window, view, advance} = harness({online: false});
  const waiting = reconnect.wait({error: new TypeError('Failed to fetch'), attempt: 0});
  assert.equal(view.shown.at(-1).reason, 'offline');
  assert.equal(view.shown.at(-1).waiting, 'offline');
  advance(10 * 60_000);
  assert.equal(await settled(waiting), false);
  window.navigator.onLine = true;
  window.dispatch('online');
  assert.equal(await settled(waiting), true);
});

test('a host that asks for a longer wait gets it, and leaving the page stops waiting', async () => {
  const {reconnect, view} = harness();
  void reconnect.wait({reason: 'busy', attempt: 0, retryAfterMs: 30_000});
  assert.equal(view.shown.at(-1).remainingMs, 30_000);
  const leaving = new AbortController();
  const waiting = reconnect.wait({reason: 'busy', attempt: 0, signal: leaving.signal});
  leaving.abort(new DOMException('left', 'AbortError'));
  await assert.rejects(waiting, {name: 'AbortError'});
});

test('each failure is named for the person by what happened', () => {
  assert.equal(reconnectReason(new TypeError('Failed to fetch'), {online: false}), 'offline');
  assert.equal(reconnectReason(Object.assign(new Error('refused'), {code: 'refused'})), 'refused');
  assert.equal(reconnectReason(Object.assign(new Error('full'), {code: 'full'})), 'busy');
  assert.equal(reconnectReason(Object.assign(new Error('slow down'), {code: 'too-many'})), 'busy');
  assert.equal(reconnectReason(new Error('fetch failed')), 'unreachable');
  assert.equal(countdownText(40_000), '40 s');
  assert.equal(countdownText(39_001), '40 s');
  assert.equal(countdownText(125_000), '2 min 05 s');
  assert.equal(countdownText(240_000), '4 min');
  assert.equal(countdownText(-5), '0 s');
});

test('the technical details name the error, tries, host and build, and never a secret', () => {
  const key = 'ab'.repeat(32);
  const token = 'Atjb0oLLuLf1fSQ5N2ExZjIzNS1jYzI2LTRjYjUtOTM1Yi1l';
  const error = Object.assign(new Error(`join refused for ${key}: certificate conflict`), {
    name: 'AdmissionError', code: 'refused', reason: `certificate conflict at /#/w/r?invite=${token}&x=1`, status: 403,
  });
  const text = reconnectDetails({reason: 'refused', error, attempt: 3, since: Date.UTC(2026, 9, 25, 7, 0, 0),
    origin: `https://lounge.example:8443/peer/v1?token=${token}`, build: '7a6b77364b9df9cf618d'});
  assert.match(text, /Reason: refused/);
  assert.match(text, /AdmissionError: join refused for \[redacted\]: certificate conflict/);
  assert.match(text, /code refused · HTTP 403/);
  assert.match(text, /invite=\[redacted\]&x=1/);
  assert.match(text, /Failed tries: 4/);
  assert.match(text, /Failing since: 2026-10-25T07:00:00.000Z/);
  assert.match(text, /Host: https:\/\/lounge.example:8443$/m);
  assert.match(text, /Page build: 7a6b77364b9df9cf618d/);
  assert.ok(!text.includes(key) && !text.includes(token), text);
  assert.equal(redact(`cert {"signature":"${'Zm9v'.repeat(20)}"}`), 'cert {"signature":"[redacted]"}');
  assert.match(reconnectDetails({reason: 'unreachable', error: null, attempt: 0}), /Last error: none[\s\S]*Host: unknown/);
});

test('the screen says each change once to screen readers, and holds Try now while trying', async () => {
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) {
      const listeners = {};
      elements.set(id, {id, textContent: '', disabled: false, dataset: {}, attributes: {},
        setAttribute(name, value) { this.attributes[name] = value; },
        addEventListener: (type, listener) => { listeners[type] = listener; }, click: () => listeners.click?.()});
    }
    return elements.get(id);
  };
  const document = {getElementById: element, documentElement: {dataset: {}}};
  let tried = 0;
  const copied = [];
  const view = domReconnectView({document, workspace: () => 'Lounge', onTryNow: () => { tried += 1; },
    messages: () => new Map([['arrival-reconnect-next', 'Nächster Versuch in { $time }']]),
    clipboard: {writeText: async text => { copied.push(text); }}});
  const state = {reason: 'unreachable', waiting: 'counting', remainingMs: 40_000, attempt: 1, since: 0,
    origin: 'https://lounge.example', error: new TypeError('Failed to fetch')};
  view.show(state);
  assert.equal(document.documentElement.dataset.reconnect, 'unreachable');
  assert.equal(element('arrival-reconnect-heading').textContent, 'Can’t reach Lounge');
  assert.equal(element('arrival-reconnect-next').textContent, 'Nächster Versuch in 40 s', 'localized');
  const said = element('arrival-reconnect-said').textContent;
  assert.match(said, /Can’t reach Lounge\. It may be restarting/);
  view.show({...state, remainingMs: 39_000});
  assert.equal(element('arrival-reconnect-next').textContent, 'Nächster Versuch in 39 s');
  element('arrival-reconnect-said').textContent = '';
  view.show({...state, remainingMs: 38_000});
  assert.equal(element('arrival-reconnect-said').textContent, '', 'the countdown is not announced each second');
  assert.equal(element('arrival-reconnect-now').disabled, false);
  element('arrival-reconnect-now').click();
  assert.equal(tried, 1);
  view.show({...state, waiting: 'trying'});
  assert.equal(element('arrival-reconnect-now').disabled, true);
  assert.equal(element('arrival-reconnect-now').attributes['aria-busy'], 'true');
  assert.equal(element('arrival-reconnect-said').textContent, 'Trying now…');
  element('arrival-reconnect-copy').click();
  await new Promise(resolve => setImmediate(resolve));
  assert.match(copied[0], /Last error: TypeError: Failed to fetch[\s\S]*Host: https:\/\/lounge.example/);
  assert.equal(element('arrival-reconnect-copy').textContent, 'Copied');
  view.hide();
  assert.equal(document.documentElement.dataset.reconnect, undefined);
});
