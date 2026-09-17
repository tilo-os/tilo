import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

function fixture() {
  const cells = Array(32).fill(0), events = {}, timers = [];
  const context = {fillStyle: '', fillRect(x) { cells[x / 4] = this.fillStyle === '#fff' ? 1 : 0; }};
  const canvas = {style: {}, getContext: () => context};
  const animation = {style: {}};
  const page = {devicePixelRatio: 2, scrollY: 0,
    document: {querySelector: selector => selector === 'canvas' ? canvas : animation},
    addEventListener: (name, callback) => events[name] = callback,
    setInterval: callback => timers.push(callback)};
  vm.createContext(page);
  const html = readFileSync(new URL('./fixtures/scroll-response.html', import.meta.url), 'utf8');
  vm.runInContext(html.match(/<script>([\s\S]*?)<\/script>/)[1], page);
  const bytes = () => [0, 1, 2, 3].map(i => cells.slice(i * 8, i * 8 + 8).reduce((n, bit) => n * 2 + bit, 0));
  return {page, events, timers, canvas, bytes};
}

test('marker acknowledges actual scroll, not wheel delivery or unrelated animation', () => {
  const f = fixture();
  assert.deepEqual(f.bytes(), [0xb3, 0, 0, 0xb3]);
  assert.equal(f.canvas.style.width, '64px');
  f.events.wheel({deltaMode: 0, deltaY: 65});
  f.timers[0]();
  assert.deepEqual(f.bytes(), [0xb3, 0, 0, 0xb3]);
  f.events.scroll(); // A scroll notification without changed position is insufficient.
  assert.deepEqual(f.bytes(), [0xb3, 0, 0, 0xb3]);
  f.page.scrollY = 65; f.events.scroll();
  assert.deepEqual(f.bytes(), [0xb3, 0, 65, 0xf2]);
  f.events.wheel({deltaMode: 0, deltaY: -65});
  f.page.scrollY = 0; f.events.scroll();
  assert.deepEqual(f.bytes(), [0xb3, 255, 191, 0xf3]);
});
test('overlapping wheel inputs invalidate acknowledgement rather than guessing', () => {
  const f = fixture();
  f.events.wheel({deltaMode: 0, deltaY: 65});
  f.events.wheel({deltaMode: 0, deltaY: 66});
  f.page.scrollY = 131; f.events.scroll();
  assert.deepEqual(f.bytes(), [0xb3, 0, 0, 0xb3]);
});
test('unsupported delta units and noninteger values cannot produce acknowledgements', () => {
  for (const event of [{deltaMode: 1, deltaY: 65}, {deltaMode: 0, deltaY: 1.5}, {deltaMode: 0, deltaY: 32768}]) {
    const f = fixture(); f.events.wheel(event);
    f.page.scrollY = 65; f.events.scroll();
    assert.deepEqual(f.bytes(), [0xb3, 0, 0, 0xb3]);
  }
});

test('movement predating wheel dispatch cannot acknowledge that wheel', () => {
  const f = fixture();
  f.page.scrollY = 65; // An older movement whose scroll notification is queued.
  f.events.wheel({deltaMode: 0, deltaY: 66});
  f.events.scroll();
  assert.deepEqual(f.bytes(), [0xb3, 0, 0, 0xb3]);
  f.page.scrollY = 131; f.events.scroll();
  assert.deepEqual(f.bytes(), [0xb3, 0, 66, 0xf1]);
});
