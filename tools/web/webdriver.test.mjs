// Tests of the WebDriver page client (tools/web/webdriver.mjs): the two pure
// pieces, the W3C actions a page step becomes and what the driver's refusal
// says. Driving Safari itself needs the macOS host the module's header names.
import assert from 'node:assert/strict';
import test from 'node:test';
import {BUTTONS, KEYS, Unavailable, launch, safaridriverUnavailable, wxActions} from './webdriver.mjs';

test('a click is the pointer moved, pressed and lifted at the point, in that order', () => {
  const [source, ...rest] = wxActions([
    {pointer: {type: 'pointerMove', x: 40, y: 90, origin: 'viewport', duration: 0, pause: 150}},
    {pointer: {type: 'pointerDown', button: BUTTONS.left, pause: 0}},
    {pointer: {type: 'pointerUp', button: BUTTONS.left, pause: 100}},
  ]);
  assert.equal(rest.length, 0, 'no keyboard source for a click alone');
  assert.equal(source.type, 'pointer');
  assert.deepEqual(source.parameters, {pointerType: 'mouse'});
  assert.deepEqual(source.actions, [
    {type: 'pointerMove', x: 40, y: 90, origin: 'viewport', duration: 0},
    {type: 'pause', duration: 150},
    {type: 'pointerDown', button: 0},
    {type: 'pointerUp', button: 0},
    {type: 'pause', duration: 100},
  ]);
});

test('a Control+V is the key held down, V pressed and lifted, then the key released', () => {
  const sources = wxActions([{key: {value: KEYS.Control.value, up: false}}, {key: {value: KEYS.v.value}}, {key: {value: KEYS.Control.value, down: false}}]);
  assert.equal(sources.length, 1);
  assert.deepEqual(sources[0].actions, [
    {type: 'keyDown', value: '\uE009'},
    {type: 'keyDown', value: 'v'},
    {type: 'keyUp', value: 'v'},
    {type: 'keyUp', value: '\uE009'},
  ]);
});

test('the pointer leads the keyboard, so a case\'s pauses are its clock', () => {
  const sources = wxActions([{key: {value: 'a'}}, {pointer: {type: 'pointerMove', x: 1, y: 2}}, {pause: 250}]);
  assert.deepEqual(sources.map(source => source.type), ['pointer', 'key']);
  assert.deepEqual(sources[0].actions, [{type: 'pointerMove', x: 1, y: 2}, {type: 'pause', duration: 250}]);
  assert.throws(() => wxActions([{jump: true}]), /a pointer step, a key step or a pause/);
});

test('a driver that will not start says what to enable', () => {
  const said = safaridriverUnavailable('/usr/bin/safaridriver', 1, 'safaridriver: remote automation is not enabled\n');
  assert.match(said, /exited \(1\)/);
  assert.match(said, /logged-in desktop session/);
  assert.match(said, /--enable/);
});

test('a host that is not macOS cannot run Safari, and says so instead of drawing one', async () => {
  await assert.rejects(launch({platform: 'linux'}), error => error instanceof Unavailable
    && /macOS's browser/.test(error.message) && /WebKitGTK, WPE/.test(error.message));
  await assert.rejects(launch({platform: 'win32'}), Unavailable);
});

test('a missing driver is unavailable, never a page result', async () => {
  const previous = process.env.SAFARIDRIVER;
  process.env.SAFARIDRIVER = '/nonexistent/safaridriver';
  try {
    await assert.rejects(launch({platform: 'darwin', startTimeout: 5000}), error => error instanceof Unavailable
      && /safaridriver did not start/.test(error.message));
  } finally {
    if (previous === undefined) delete process.env.SAFARIDRIVER; else process.env.SAFARIDRIVER = previous;
  }
});
