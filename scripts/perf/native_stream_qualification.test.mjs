// Tests of the native stream qualification's reading and judging
// (scripts/perf/native_stream_qualification.mjs); the run itself needs a
// host, the session example, a built bundle and Chrome, which
// Perf / scheduled's `stream` job has.
import assert from 'node:assert/strict';
import test from 'node:test';
import {CODE_BITS, analyse, codeCheck, codeGeometry, judgeGoals, parseArgs, readCode} from './native_stream_qualification.mjs';
import {stats} from './browser_qualification.mjs';
import {focusDiscipline, inputsOf, judgeIme, judgePaste, judgeStream, pointFor} from './native_stream_acceptance.mjs';

/** Three RGBA rows through the number blocks of frame `index`, as the session draws them. */
function rows(index, width, {flip} = {}) {
  const {pitch, side, offset} = codeGeometry(width);
  const pixels = new Uint8ClampedArray(width * 3 * 4).fill(128);
  let bits = index * 16 + codeCheck(index);
  if (flip !== undefined) bits ^= 1 << (CODE_BITS - 1 - flip);
  for (let bit = 0; bit < CODE_BITS; bit++) {
    const shade = Math.floor(bits / 2 ** (CODE_BITS - 1 - bit)) % 2 ? 235 : 16;
    for (let y = 0; y < 3; y++) {
      for (let x = offset + bit * pitch; x < offset + bit * pitch + side; x++) {
        pixels.set([shade, shade, shade, 255], (y * width + x) * 4);
      }
    }
  }
  return pixels;
}

test('the check is the session\'s: nibbles xored with 0xA', () => {
  // legacy_stream_source.rs `code_check`, worked by hand.
  assert.equal(codeCheck(0), 0xa);
  assert.equal(codeCheck(0x1234), (0x1 ^ 0x2 ^ 0x3 ^ 0x4 ^ 0xa) & 0xf);
  assert.equal(codeCheck(0xffff), 0xa);
});

test('every frame number reads back from its blocks, at any window width', () => {
  for (const width of [640, 1280, 1920]) {
    for (const index of [0, 1, 255, 4097, 0xbeef, 0xffff]) assert.equal(readCode(rows(index, width), width), index);
  }
});

test('a picture with one wrong block is corrupt, never another frame', () => {
  for (let flip = 0; flip < CODE_BITS; flip++) assert.equal(readCode(rows(300, 1280, {flip}), 1280), null);
});

test('arguments: required pieces, carrier, codec, window and defaults', () => {
  const base = ['bundle', '--out', 'r.json', '--relay', 'relay', '--source', 'source', '--renderer', 'swiftshader'];
  const options = parseArgs(base);
  assert.equal(options.carrier, 'webtransport');
  assert.deepEqual(options.window, {width: 1280, height: 720});
  assert.equal(options.frameTimeout, 120);
  assert.deepEqual(parseArgs([...base, '--window', '640x360', '--carrier', 'https']).window, {width: 640, height: 360});
  assert.throws(() => parseArgs(base.slice(0, -2)), /usage/);
  assert.throws(() => parseArgs([...base, '--carrier', 'carrier-pigeon']), /--carrier/);
  assert.equal(options.codec, 'h264', 'H.264 by default');
  assert.equal(parseArgs([...base, '--codec', 'vp9']).codec, 'vp9');
  assert.throws(() => parseArgs([...base, '--codec', 'hevc-main10']), /--codec/);
  assert.throws(() => parseArgs([...base, '--window', '641x360']), /even/);
});

/** A run: `n` frames 33 ms apart from t0, each drawn `lag` µs after capture. */
function run(n, {lag = 40_000, drop = [], mutate = draws => draws, presses = 3, arrive = 3} = {}) {
  const t0 = 1_760_000_000_000_000;
  const sent = Array.from({length: n}, (_, index) => ({index, captureUs: t0 + index * 33_333, publishUs: t0 + index * 33_333 + 900,
    encodeUs: 4000, bytes: 9000, key: index % 30 === 0}));
  const draws = mutate(sent.filter(frame => !drop.includes(frame.index)).map(frame => ({wallUs: frame.captureUs + lag,
    tsUs: frame.captureUs, index: frame.index, visible: true, width: 1280, height: 720})));
  const press = Array.from({length: presses}, (_, i) => t0 + 10_000_000 + i * 200_000);
  return {
    sent, draws, start: {startUs: t0 - 300_000}, stop: {stopUs: sent.at(-1).captureUs, lastIndex: n - 1, frames: n},
    presses: press, arrivals: press.slice(0, arrive).map(at => at + 4000),
    decoder: {created: 1, configs: ['avc1.64001F'], chunks: n, keys: 1, outputs: n, errors: []}, control: true,
  };
}

test('a clean run is correct, with startup, latency, delivery and input measured', () => {
  const result = analyse(run(90));
  assert.deepEqual(result.correctness.failures, []);
  assert.equal(result.correctness.converged, true);
  assert.deepEqual(result.samples.startup_ms, [340]);
  assert.equal(result.samples.frame_latency_ms.length, 90);
  assert.ok(result.samples.frame_latency_ms.every(ms => ms === 40));
  assert.equal(result.counts.deliveredPercent, 100);
  assert.equal(result.counts.drawnFps, 30);
  assert.deepEqual(result.samples.input_ms, [4, 4, 4]);
});

test('dropped frames lower delivery but are not incorrect', () => {
  const result = analyse(run(100, {drop: [10, 11, 50]}));
  assert.deepEqual(result.correctness.failures, []);
  assert.equal(result.counts.deliveredPercent, 97);
});

test('corrupt, misnamed and older pictures, an unconverged end and lost presses are failures', () => {
  const failures = mutate => analyse(run(60, {mutate})).correctness.failures.join('; ');
  assert.match(failures(draws => [...draws, {...draws[5], index: null}]), /1 corrupt pictures/);
  assert.match(failures(draws => draws.map((draw, i) => i === 7 ? {...draw, tsUs: draw.tsUs + 1} : draw)), /1 pictures are not the frame/);
  assert.match(failures(draws => [...draws.slice(0, 30), draws[3], ...draws.slice(30)]), /1 pictures went back/);
  assert.match(failures(draws => draws.slice(0, -1)), /ended on frame 58, not the last sent 59/);
  assert.match(analyse(run(60, {arrive: 2})).correctness.failures.join(), /2 of 3 presses/);
  assert.match(analyse({...run(60), control: false}).correctness.failures.join(), /may not drive/);
  assert.match(analyse({...run(60), decoder: {...run(60).decoder, errors: ['EncodingError']}}).correctness.failures.join(),
    /1 decoder errors: EncodingError/);
});

test('startup counts the first picture shown on the desktop, not one drawn while hidden', () => {
  const result = analyse(run(30, {mutate: draws => draws.map((draw, i) => ({...draw, visible: i >= 4}))}));
  assert.deepEqual(result.samples.startup_ms, [(4 * 33_333 + 40_000 + 300_000) / 1000]);
  assert.equal(result.counts.firstPictureMs, 340);
});

test('the warm-up is judged but not timed', () => {
  // The first 30 frames (1 s) are the warm-up: drawn late and with gaps.
  const input = run(90, {drop: [3, 4, 5], mutate: draws => draws.map(draw => draw.index < 30 ? {...draw, wallUs: draw.wallUs + 2_000_000} : draw)});
  const cut = analyse({...input, measureFromUs: input.sent[30].captureUs});
  assert.equal(cut.samples.frame_latency_ms.length, 60);
  assert.ok(cut.samples.frame_latency_ms.every(ms => ms === 40));
  assert.equal(cut.counts.deliveredPercent, 100);
  assert.equal(cut.samples.encode_ms.length, 60);
  // A corrupt picture in the warm-up still fails the run.
  const corrupt = analyse({...input, draws: [{...input.draws[1], index: null}, ...input.draws], measureFromUs: input.sent[30].captureUs});
  assert.match(corrupt.correctness.failures.join(), /1 corrupt pictures/);
});

test('goals are judged against the summary, delivery from below', () => {
  const result = analyse(run(90, {lag: 150_000, drop: [3, 4]}));
  const summary = Object.fromEntries(Object.entries(result.samples).map(([name, values]) => [name, stats(values)]));
  const goals = Object.fromEntries(judgeGoals(summary, result.counts).map(goal => [goal.name, goal.met]));
  assert.deepEqual(goals, {
    'startup to the first picture (ms)': true,
    'frame latency, capture to picture (p95, ms)': false,
    'frames delivered (%)': false,
    'input, click to session (p95, ms)': true,
  });
});

// --- the acceptance cases' judging (native_stream_acceptance.mjs) ----------------

const input = value => JSON.stringify({event: 'input', arrivalUs: 1, surface: 7, input: '', value});
const key = (keysym, pressed) => input({Key: {keysym, pressed, modifiers: 0}});
const focus = focused => input({KeyboardFocus: {surface: 7, focused}});

test('a session\'s input is read from its event lines only', () => {
  const lines = ['{"event":"ready"}', 'not json', key(30, true), '{"event":"stopped"}', focus(false)];
  assert.deepEqual(inputsOf(lines).map(found => found.value), [{Key: {keysym: 30, pressed: true, modifiers: 0}},
    {KeyboardFocus: {surface: 7, focused: false}}]);
  assert.equal(inputsOf(lines, 3).length, 1);
  assert.equal(inputsOf(lines, 0, 3).length, 1);
});

test('keys and pastes count as stray only while the window does not hold the keyboard', () => {
  const held = [focus(true), key(30, true), key(30, false), input({ClipboardText: {text: 'x'}}), focus(false)];
  assert.deepEqual(focusDiscipline(inputsOf(held)), []);
  // Typing before any focus, or after it moved away, reached the wrong window.
  assert.deepEqual(focusDiscipline(inputsOf([key(48, true), key(48, false)])), ['key 48']);
  assert.deepEqual(focusDiscipline(inputsOf([...held, key(18, true), input({ClipboardText: {text: 'y'}})])), ['key 18', 'pasted text']);
  assert.deepEqual(focusDiscipline(inputsOf([input({Text: {text: '日本'}})])), ['committed text']);
  // Releases sent while the keyboard is let go are not typing.
  assert.deepEqual(focusDiscipline(inputsOf([focus(true), focus(false), key(30, false)])), []);
});

/** A second window's frames: 640x360, then 800x448 from frame `resizeAt`, drawn once each on canvas `epoch` 1. */
function second(n, resizeAt = n) {
  const sent = Array.from({length: n}, (_, index) => ({index, captureUs: 1000 + index * 66_666,
    width: index < resizeAt ? 640 : 800, height: index < resizeAt ? 360 : 448}));
  const draws = sent.map(frame => ({index: frame.index, tsUs: frame.captureUs, width: frame.width, height: frame.height, epoch: 1}));
  return {sent, draws, lastIndex: n - 1};
}

test('a resized window is judged at each frame\'s own size', () => {
  const clean = judgeStream(second(30, 20));
  assert.deepEqual(clean.failures, []);
  assert.deepEqual(clean.finalSize, {width: 800, height: 448});
  const resized = second(30, 20);
  const stretched = judgeStream({...resized, draws: resized.draws.map(draw => draw.index === 25 ? {...draw, width: 640, height: 360} : draw)});
  assert.match(stretched.failures.join(), /1 pictures drawn at a size other than their frame's/);
});

test('a window opened again may start over from its replayed keyframe, but never go back on one canvas', () => {
  const window = second(30);
  // The live plane came back: the window's new canvas replays from frame 10.
  const reopened = [...window.draws.slice(0, 15), ...window.draws.slice(10).map(draw => ({...draw, epoch: 2}))];
  assert.deepEqual(judgeStream({...window, draws: reopened}).failures, []);
  assert.equal(judgeStream({...window, draws: reopened}).epochs, 2);
  const back = [...window.draws.slice(0, 15), ...window.draws.slice(10)];
  // Frames 10 to 13 after 14, on the same canvas.
  assert.match(judgeStream({...window, draws: back}).failures.join(), /4 pictures went back/);
});

test('a window judged on a page that drew none, or a corrupt or unconverged one, fails', () => {
  const window = second(10);
  assert.match(judgeStream({...window, draws: []}).failures.join(), /no picture of the window/);
  assert.match(judgeStream({...window, draws: [...window.draws, {...window.draws[2], index: null}]}).failures.join(), /1 corrupt/);
  assert.match(judgeStream({...window, draws: window.draws.slice(0, -2)}).failures.join(), /ended on frame 7, not the last sent 9/);
  assert.match(judgeStream({...window, draws: window.draws.map((draw, i) => i === 4 ? {...draw, tsUs: 1} : draw)}).failures.join(),
    /1 pictures are not the frame/);
});

test('a paste is the text, exactly, then Control+V', () => {
  const text = 'two\nlines ✓';
  const clip = input({ClipboardText: {text}});
  assert.deepEqual(judgePaste(inputsOf([key(29, true), clip, key(47, true), key(47, false), key(29, false)]), text), []);
  assert.deepEqual(judgePaste(inputsOf([key(29, true), key(47, true)]), text), ['the pasted text did not reach the window']);
  assert.match(judgePaste(inputsOf([key(29, true), input({ClipboardText: {text: 'two lines'}}), key(47, true)]), text).join(),
    /offered "two lines"/);
  assert.match(judgePaste(inputsOf([key(29, true), key(47, true), key(47, false), clip, key(29, false)]), text).join(),
    /before the text it pastes.*no paste chord/);
  assert.match(judgePaste(inputsOf([clip, key(47, true), key(47, false)]), text).join(), /no paste chord/);
});

test('a window pixel maps into its letterboxed box', () => {
  // 800x400 into a 400x400 box: scale 0.5, bars of 100 px above and below.
  assert.deepEqual(pointFor({left: 10, top: 20, width: 400, height: 400}, 800, 400, 200, 100), {x: 110, y: 170, scale: 0.5});
  // 640x360 into 1280x1000: scale 2, bars of 140 px.
  assert.deepEqual(pointFor({left: 0, top: 0, width: 1280, height: 1000}, 640, 360, 0, 0), {x: 0, y: 140, scale: 2});
});

test('an input method reaches the window as its committed text, once, and none of its keys', () => {
  const text = value => input({Text: {text: value}});
  assert.deepEqual(judgeIme(inputsOf([text('日本'), text('🙂')]), ['日本', '🙂']), []);
  assert.match(judgeIme(inputsOf([text('日本'), text('日本'), text('🙂')]), ['日本', '🙂']).join(), /got the committed text \["日本","日本","🙂"\]/);
  assert.match(judgeIme(inputsOf([text('に'), text('日本'), text('🙂')]), ['日本', '🙂']).join(), /not \["日本","🙂"\]/);
  assert.match(judgeIme(inputsOf([key(49, true), key(49, false), text('日本'), text('🙂')]), ['日本', '🙂']).join(),
    /1 keys of the composition reached the window: 49/);
  assert.match(judgeIme(inputsOf([]), ['日本', '🙂']).join(), /got the committed text \[\]/);
});
