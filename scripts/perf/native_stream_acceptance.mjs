// The acceptance cases of a native window on the browser desktop, driven
// headlessly by scripts/perf/native_stream_qualification.mjs after its timed
// run, on the same host, governed room and desktop. Every case is judged, and
// one that breaks fails the run with the rest of its correctness; a case the
// harness cannot drive (a window that never comes on screen to click, a page
// with no live session to drop) makes the run invalid, never a pass.
//
// The first window (the timed run's, still by now) is W1. The cases, in order:
//
//   focus      a key typed into W1 reaches it; a click on the desktop beside
//              it takes the keyboard away (W1 is told, and a key typed then
//              never reaches it); a click on W1 gives the keyboard back
//   paste      text on the page's clipboard, pasted with Control+V into W1,
//              reaches it as the session's clipboard before the paste chord
//              (with Control held), byte for byte
//   resize     the page is resized: W1 follows its tile, and a click on a
//              window pixel lands on that pixel; then a second window W2
//              changes its own size while it streams: every page draws each
//              picture at its frame's size and ends on the last frame at the
//              new size
//   fallback   a second device of the owner on a network where WebTransport
//              never becomes ready takes the HTTPS stream, shows W1's still
//              picture on arrival, follows W2 to its last frame and drives it
//   viewer     a second person, invited as a viewer and admitted by the
//              owner, shows W1's still picture and follows W2 to its last
//              frame, but is offered no control: a click and a key on the
//              window, and input sent to the host directly, reach no window
//   recovery   the first page's live plane is dropped while W2 streams: the
//              page reconnects and W2 resumes with newer frames, never a
//              corrupt one, and ends on its last
//   stale      W2 opening takes the keyboard from W1, and a key typed into
//              W2 reaches only W2; once W2 closes, nothing typed reaches it
//              (the page removed it and the host refuses input for it), and
//              W1 takes the keyboard again when clicked. Over the whole run,
//              no key and no paste reaches a window that did not hold the
//              keyboard when it arrived
//
// Not driven here, with the reason (docs/measurements/performance.md):
//   Safari     WebKit's only Linux builds are WebKitGTK/WPE, not Safari, and
//              lack WebTransport; Safari runs only on macOS under a logged-in
//              user with remote automation enabled (`safaridriver --enable`)
//   IME        the legacy-window protocol carries no committed text
//              (`legacy_window::InputEvent`), and the page forwards physical
//              keys only: a composition on the page reaches the window as the
//              keys typed, not as the composed text
import {sleep} from '../../tools/web/chrome.mjs';
import {Invalid} from './browser_qualification.mjs';

/** Keys the cases type: CDP key event fields and the evdev code the window gets. */
export const KEYS = Object.freeze({
  a: {key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, text: 'a', evdev: 30},
  b: {key: 'b', code: 'KeyB', windowsVirtualKeyCode: 66, text: 'b', evdev: 48},
  c: {key: 'c', code: 'KeyC', windowsVirtualKeyCode: 67, text: 'c', evdev: 46},
  d: {key: 'd', code: 'KeyD', windowsVirtualKeyCode: 68, text: 'd', evdev: 32},
  e: {key: 'e', code: 'KeyE', windowsVirtualKeyCode: 69, text: 'e', evdev: 18},
  f: {key: 'f', code: 'KeyF', windowsVirtualKeyCode: 70, text: 'f', evdev: 33},
  z: {key: 'z', code: 'KeyZ', windowsVirtualKeyCode: 90, text: 'z', evdev: 44},
});
const CONTROL = 29, CONTROL_RIGHT = 97, KEY_V = 47, LEFT_BUTTON = 272;
/** CDP's modifier bit for Control. */
const CTRL = 2;
/** What the paste case puts on the clipboard: two lines, beyond ASCII. */
export const PASTED = 'Pasted into a native window ✓\nzweite Zeile, 日本語';
/** The second window: small (three decoders share the runner), 15 fps, resized two thirds in. */
export const SECOND = Object.freeze({surface: 8, width: 640, height: 360, fps: 15, seconds: 30, resizeAt: 20,
  resizeTo: Object.freeze({width: 800, height: 448})});

// --- judging (pure, tested) ------------------------------------------------------

/** The input events in a session's stdout `lines[from, to)`: `{arrivalUs, value}`. */
export function inputsOf(lines, from = 0, to = lines.length) {
  const out = [];
  for (const line of lines.slice(from, to)) {
    let parsed;
    try { parsed = JSON.parse(line); } catch { continue; }
    if (parsed?.event === 'input') out.push({arrivalUs: parsed.arrivalUs, value: parsed.value});
  }
  return out;
}

const keyOf = input => input.value?.Key ?? null;
const focusOf = input => input.value?.KeyboardFocus?.focused ?? null;
export const isKey = (keysym, pressed) => input => keyOf(input)?.keysym === keysym && keyOf(input)?.pressed === pressed;
export const isFocus = focused => input => focusOf(input) === focused;
export const isButton = (button, pressed) => input =>
  input.value?.PointerButton?.button === button && input.value?.PointerButton?.pressed === pressed;

/**
 * Keys pressed and text pasted while the window did not hold the keyboard,
 * by the `KeyboardFocus` events it was sent: typing that reached a window
 * other than the one the person was typing into.
 */
export function focusDiscipline(inputs) {
  let focused = false;
  const stray = [];
  for (const input of inputs) {
    const focus = focusOf(input);
    if (focus !== null) { focused = focus; continue; }
    const key = keyOf(input);
    if (!focused && (key?.pressed || input.value?.ClipboardText)) stray.push(key ? `key ${key.keysym}` : 'pasted text');
  }
  return stray;
}

/**
 * One window's pictures on one page, judged against what its session sent
 * (`sent`, frames with their size) and the frame it ended on (`lastIndex`):
 * every picture readable, the frame its timestamp names and drawn at that
 * frame's size, never older than the one before it on the same canvas (a
 * window opened again starts a new one), and the last one the last frame.
 */
export function judgeStream({sent, draws, lastIndex}) {
  const captured = new Map(sent.map(frame => [frame.index, frame]));
  const valid = draws.filter(draw => draw.index !== null);
  const corrupt = draws.length - valid.length;
  const misnamed = valid.filter(draw => captured.get(draw.index)?.captureUs !== draw.tsUs).length;
  const misSized = valid.filter(draw => {
    const frame = captured.get(draw.index);
    return frame?.width !== undefined && (frame.width !== draw.width || frame.height !== draw.height);
  }).length;
  let backwards = 0;
  const latest = new Map();
  for (const draw of valid) {
    const before = latest.get(draw.epoch) ?? -1;
    if (draw.index < before) backwards++;
    latest.set(draw.epoch, Math.max(before, draw.index));
  }
  const final = valid.at(-1) ?? null;
  const failures = [
    ...(valid.length ? [] : ['no picture of the window was drawn']),
    ...(corrupt ? [`${corrupt} corrupt pictures (number check failed)`] : []),
    ...(misnamed ? [`${misnamed} pictures are not the frame their timestamp names`] : []),
    ...(misSized ? [`${misSized} pictures drawn at a size other than their frame's`] : []),
    ...(backwards ? [`${backwards} pictures went back to an older frame`] : []),
    ...(final && final.index !== lastIndex ? [`ended on frame ${final.index}, not the last sent ${lastIndex}`] : []),
  ];
  return {failures, pictures: valid.length, distinct: new Set(valid.map(draw => draw.index)).size, corrupt, misnamed,
    misSized, backwards, final: final?.index ?? null, finalSize: final ? {width: final.width, height: final.height} : null,
    epochs: new Set(draws.map(draw => draw.epoch)).size};
}

/**
 * A paste as the window received it: the text offered as its clipboard,
 * exactly, then the paste chord (V pressed while Control is held), so the
 * window's own paste reads that text.
 */
export function judgePaste(inputs, text) {
  const offered = inputs.findIndex(input => input.value?.ClipboardText !== undefined);
  if (offered < 0) return ['the pasted text did not reach the window'];
  const failures = [];
  const got = inputs[offered].value.ClipboardText.text;
  if (got !== text) failures.push(`the window was offered ${JSON.stringify(got)}, not ${JSON.stringify(text)}`);
  const held = new Set();
  let chord = false, early = false;
  inputs.forEach((input, at) => {
    const key = keyOf(input);
    if (!key) return;
    if (key.keysym === KEY_V && key.pressed) {
      if (at < offered) early = true;
      else if (held.has(CONTROL) || held.has(CONTROL_RIGHT)) chord = true;
    }
    if (key.pressed) held.add(key.keysym); else held.delete(key.keysym);
  });
  if (early) failures.push('the paste chord reached the window before the text it pastes');
  if (!chord) failures.push('no paste chord (Control+V) followed the text');
  return failures;
}

/**
 * The client point that shows window pixel (`x`, `y`) of a `width`×`height`
 * picture letterboxed (`object-fit: contain`) into `box`: worked out here
 * from the box, independently of the page's own mapping.
 */
export function pointFor(box, width, height, x, y) {
  const scale = Math.min(box.width / width, box.height / height);
  return {x: box.left + (box.width - width * scale) / 2 + x * scale, y: box.top + (box.height - height * scale) / 2 + y * scale, scale};
}

// --- in the page -----------------------------------------------------------------

const keyed = prefix => `[...document.querySelectorAll('canvas.tilo-legacy-window')].find(c => c.dataset.key?.startsWith(${JSON.stringify(prefix)}))`;

/** The window streamed by `prefix`'s device on the page: where it is, its picture's size, whether this member drives it. */
const windowAt = prefix => `(() => { const c = ${keyed(prefix)};
  if (!c || c.style.display !== 'block') return null; const r = c.getBoundingClientRect();
  return {x: r.left + r.width / 2, y: r.top + r.height / 2, left: r.left, top: r.top, width: r.width, height: r.height,
    pictureWidth: c.width, pictureHeight: c.height, control: c.style.pointerEvents === 'auto', key: c.dataset.key}; })()`;

/** A point of the desktop beside the window (below, right of, left of or above it), not on any window's canvas. */
const besideWindow = prefix => `(() => { const c = ${keyed(prefix)}; if (!c) return null;
  const r = c.getBoundingClientRect(), w = innerWidth, h = innerHeight, desktop = globalThis.tiloMount?.canvas;
  const candidates = [[r.left + r.width / 2, (r.bottom + h) / 2], [(r.right + w) / 2, r.top + r.height / 2],
    [r.left / 2, r.top + r.height / 2], [r.left + r.width / 2, r.top / 2]];
  for (const [x, y] of candidates) {
    if (x < 2 || y < 2 || x > w - 2 || y > h - 2) continue;
    if (x > r.left - 24 && x < r.right + 24 && y > r.top - 24 && y < r.bottom + 24) continue;
    if (document.elementFromPoint(x, y) === desktop) return {x, y};
  }
  return null; })()`;

const drawsOf = prefix => `globalThis.__tiloStream.draws.filter(d => d.key?.startsWith(${JSON.stringify(prefix)}))`;
const lastDrawn = prefix => `globalThis.__tiloStream.draws.filter(d => d.index !== null && d.key?.startsWith(${JSON.stringify(prefix)})).at(-1) ?? null`;

// --- driving ---------------------------------------------------------------------

const nowUs = () => Math.round((performance.timeOrigin + performance.now()) * 1000);

async function typeKey(page, {evdev: _evdev, ...key}) {
  await page.send('Input.dispatchKeyEvent', {type: 'keyDown', ...key});
  await page.send('Input.dispatchKeyEvent', {type: 'keyUp', key: key.key, code: key.code, windowsVirtualKeyCode: key.windowsVirtualKeyCode});
}

async function click(page, {x, y}) {
  await page.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  await sleep(150);
  await page.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
  await page.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
}

/** `probe()`'s first truthy value within `ms`, else null. */
async function eventually(probe, ms, every = 100) {
  const deadline = Date.now() + ms;
  for (;;) {
    const value = await probe().catch(() => null);
    if (value) return value;
    if (Date.now() > deadline) return null;
    await sleep(every);
  }
}

/** The first input since line `from` of `session` that `match` accepts, within `ms`, else null. */
const waitInput = (session, from, match, ms) =>
  eventually(async () => inputsOf(session.source.lines, from).find(match) ?? null, ms, 50);

/** The last frame `page` drew of `prefix`'s window once it is `lastIndex`, or whatever it is after `ms`. */
async function settleOn(page, prefix, lastIndex, ms) {
  const reached = await eventually(async () => (await page.evaluate(lastDrawn(prefix)))?.index === lastIndex, ms, 200);
  return reached ? lastIndex : (await page.evaluate(lastDrawn(prefix)).catch(() => null))?.index ?? null;
}

/** The window's box once it differs from `before` and has held still for a second. */
async function settledBox(page, prefix, before, ms) {
  const same = (a, b) => a && b && ['left', 'top', 'width', 'height'].every(side => Math.abs(a[side] - b[side]) < 0.5);
  let last = null, since = 0;
  return eventually(async () => {
    const box = await page.evaluate(windowAt(prefix));
    if (!box || same(box, before)) { last = null; return null; }
    if (!same(box, last)) { last = box; since = Date.now(); return null; }
    return Date.now() - since >= 1000 ? box : null;
  }, ms, 100);
}

/** Admit everyone waiting to join the owner's room until `stop()` resolves; resolves to how many. */
async function admitWhile(owner, done) {
  let admitted = 0, finished = false;
  done.finally(() => { finished = true; }).catch(() => {});
  while (!finished) {
    const waiting = await owner.evaluate('globalThis.peer.membership.requests()').catch(() => []);
    for (const person of waiting ?? []) {
      await owner.evaluate(`globalThis.peer.membership.admit(${JSON.stringify(person.requestId)})`);
      admitted++;
    }
    await sleep(300);
  }
  return admitted;
}

/**
 * The cases, in order. `ctx`: `main` the timed run's page (its window W1
 * focused by the run's clicks), `w1` its session (`{source, session, stop}`),
 * `browser`, `pagesOrigin`, `viewport`, `room`, `live` (the page's live state
 * expression), `openPage(name, params, {block, context})`, `startSession(name,
 * options)` and `readSent(log)`. Resolves to `{cases, children}`: each case's
 * findings and `failures`, and the extra sessions to stop.
 */
export async function runAcceptance(ctx) {
  const {main, w1} = ctx;
  const cases = {};
  const open = name => (cases[name] = {failures: []});
  // Where the cases are, for a run that ends invalid part of the way.
  const step = what => console.error(`acceptance: ${what}`);
  const prefix1 = `${w1.session.device}:`;
  const lines1 = () => w1.source.lines.length;

  // focus ----------------------------------------------------------------------
  step('focus');
  const focus = open('focus');
  let mark = lines1();
  await typeKey(main, KEYS.a);
  if (!await waitInput(w1, mark, isKey(KEYS.a.evdev, false), 30_000)) focus.failures.push('a key typed into the focused window did not reach it');
  const beside = await main.evaluate(besideWindow(prefix1));
  if (!beside) throw new Invalid(`no point of the desktop beside the window to click: ${JSON.stringify(await main.evaluate(windowAt(prefix1)))}`);
  focus.desktopPoint = beside;
  mark = lines1();
  await click(main, beside);
  if (!await waitInput(w1, mark, isFocus(false), 30_000)) focus.failures.push('a click on the desktop did not take the keyboard from the window');
  const away = lines1();
  await typeKey(main, KEYS.b);
  await sleep(1500);
  const back = await eventually(() => main.evaluate(windowAt(prefix1)), 10_000);
  if (!back?.control) throw new Invalid(`the window was not on screen to click again after the desktop took the keyboard: ${JSON.stringify(back)}`);
  mark = lines1();
  await click(main, back);
  const regained = await waitInput(w1, mark, isFocus(true), 30_000);
  await typeKey(main, KEYS.c);
  if (!regained) focus.failures.push('a click on the window did not give it the keyboard back');
  if (!await waitInput(w1, mark, isKey(KEYS.c.evdev, false), 30_000)) focus.failures.push('a key typed after clicking the window again did not reach it');
  if (inputsOf(w1.source.lines, away, mark).some(isKey(KEYS.b.evdev, true))) focus.failures.push('a key typed on the desktop reached the window it had left');

  // paste ----------------------------------------------------------------------
  step('paste');
  const paste = open('paste');
  await ctx.browser.send('Browser.grantPermissions', {origin: ctx.pagesOrigin, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']});
  paste.clipboard = await main.evaluate(`Promise.race([navigator.clipboard.writeText(${JSON.stringify(PASTED)})
    .then(() => navigator.clipboard.readText()).then(text => text === ${JSON.stringify(PASTED)} ? 'holds the text' : 'holds other text', error => String(error)),
    new Promise(done => setTimeout(() => done('did not answer within 5 s'), 5000))])`);
  const pastes = await main.evaluate('globalThis.__tiloStream.pastes');
  mark = lines1();
  await main.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: CTRL});
  await main.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', key: 'v', code: 'KeyV', windowsVirtualKeyCode: 86, modifiers: CTRL,
    ...(paste.clipboard === 'holds the text' ? {commands: ['paste']} : {})});
  paste.path = 'clipboard';
  if (!await eventually(async () => (await main.evaluate('globalThis.__tiloStream.pastes')) > pastes, 3000)) {
    // A browser whose clipboard does not work headless runs no paste for
    // the chord: the page's paste event carries the text instead, as the
    // browser's own would.
    paste.path = 'paste event';
    await main.evaluate(`(() => { const data = new DataTransfer(); data.setData('text/plain', ${JSON.stringify(PASTED)});
      document.activeElement.dispatchEvent(new ClipboardEvent('paste', {clipboardData: data, bubbles: true, cancelable: true})); })()`);
  } else if ((await main.evaluate('globalThis.__tiloStream.pasted')) !== PASTED) {
    throw new Invalid(`the browser pasted ${JSON.stringify(await main.evaluate('globalThis.__tiloStream.pasted'))} from a clipboard that ${paste.clipboard}`);
  }
  await main.send('Input.dispatchKeyEvent', {type: 'keyUp', key: 'v', code: 'KeyV', windowsVirtualKeyCode: 86, modifiers: CTRL});
  await main.send('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: 0});
  await waitInput(w1, mark, isKey(CONTROL, false), 30_000);
  paste.failures.push(...judgePaste(inputsOf(w1.source.lines, mark), PASTED));

  // resize: the page ---------------------------------------------------------------
  step('resize the page');
  const resize = open('resize');
  const before = await main.evaluate(windowAt(prefix1));
  const viewport = {width: Math.round(ctx.viewport.width * 1.25 / 2) * 2, height: Math.round(ctx.viewport.height * 1.2 / 2) * 2, deviceScaleFactor: 1};
  await main.send('Emulation.setDeviceMetricsOverride', {...viewport, mobile: false});
  resize.viewport = {from: ctx.viewport, to: viewport};
  const after = await settledBox(main, prefix1, before, 30_000);
  if (!after) resize.failures.push(`the window did not follow its tile when the page was resized (still ${JSON.stringify(before)})`);
  else {
    const target = [Math.round(after.pictureWidth / 4), Math.round(after.pictureHeight / 4)];
    const at = pointFor(after, after.pictureWidth, after.pictureHeight, ...target);
    mark = lines1();
    await click(main, at);
    const pressed = await waitInput(w1, mark, isButton(LEFT_BUTTON, true), 30_000);
    const inputs = inputsOf(w1.source.lines, mark);
    const motion = inputs.slice(0, inputs.indexOf(inputs.find(isButton(LEFT_BUTTON, true)))).findLast(input => input.value?.PointerMotion)?.value.PointerMotion;
    const slack = 1 / at.scale + 1;
    resize.click = {target, arrived: motion ? [motion.x, motion.y] : null, box: after};
    if (!pressed) resize.failures.push('a click on the window after the resize did not reach it');
    else if (!motion || Math.abs(motion.x - target[0]) > slack || Math.abs(motion.y - target[1]) > slack) {
      resize.failures.push(`a click on window pixel ${target.join(',')} after the resize arrived at ${motion ? `${motion.x},${motion.y}` : 'no position'}`);
    }
  }

  // the other members ----------------------------------------------------------------
  step('open the fallback and viewer pages');
  const fallback = open('fallback');
  const viewer = open('viewer');
  const invitation = await main.evaluate(`globalThis.peer.createInvitation({role: 'viewer', expiresIn: 3600})`);
  const fallbackPage = ctx.openPage('fallback', {room: ctx.room, label: 'Owner on HTTPS'}, {block: true});
  const viewerPage = ctx.openPage('viewer', {invite: invitation, label: 'Viewer'}, {context: true});
  const admitted = admitWhile(main, viewerPage);
  const [other, watcher] = await Promise.all([fallbackPage, viewerPage]);
  viewer.admitted = await admitted;
  const pages = [['first', main], ['fallback', other], ['viewer', watcher]];
  const transport = await other.evaluate('globalThis.__tiloStream.transport');
  if (!transport.blocked) throw new Invalid(`the page on a network without WebTransport never tried it: ${JSON.stringify(transport)}`);
  fallback.carrier = JSON.parse(await other.evaluate(ctx.live)).live?.carrier ?? null;
  if (fallback.carrier !== 'https-stream') fallback.failures.push(`with WebTransport never ready the page's live plane took ${fallback.carrier}, not the HTTPS stream`);
  for (const [name, page, found] of [['fallback', other, fallback], ['viewer', watcher, viewer]]) {
    const final = await settleOn(page, prefix1, w1.stop.lastIndex, 120_000);
    found.stillPicture = final;
    if (final !== w1.stop.lastIndex) found.failures.push(`the ${name} page, arriving once the window was still, shows frame ${final}, not its last ${w1.stop.lastIndex}`);
  }

  // the second window ------------------------------------------------------------------
  step('the second window');
  const stale = open('stale');
  const recovery = open('recovery');
  const w2 = await ctx.startSession('second', {surface: SECOND.surface, seconds: SECOND.seconds, width: SECOND.width, height: SECOND.height,
    fps: SECOND.fps, extra: ['--resize-at', String(SECOND.resizeAt), '--resize-to', `${SECOND.resizeTo.width}x${SECOND.resizeTo.height}`]});
  const prefix2 = `${w2.session.device}:`;
  const lines2 = () => w2.source.lines.length;
  const beforeSecond = lines1();
  w2.source.process.stdin.write('start\n');
  const started = JSON.parse(await w2.source.line(line => { try { return JSON.parse(line).event === 'started'; } catch { return false; } },
    10_000, 'the second stream start'));
  const shown = await eventually(() => main.evaluate(windowAt(prefix2)), 60_000);
  if (!shown) throw new Invalid('the second window never came on screen on the first page');
  let taken = await waitInput(w2, 0, isFocus(true), 3000);
  stale.focusedOnOpen = Boolean(taken);
  if (!taken && shown.control) { await click(main, shown); taken = await waitInput(w2, 0, isFocus(true), 30_000); }
  if (!taken) stale.failures.push('the second window never took the keyboard');
  if (!await waitInput(w1, beforeSecond, isFocus(false), 30_000)) stale.failures.push('the first window was not told it lost the keyboard when the second took it');
  const typed2 = lines2();
  await typeKey(main, KEYS.d);
  if (!await waitInput(w2, typed2, isKey(KEYS.d.evdev, false), 30_000)) stale.failures.push('a key typed into the newly focused window did not reach it');
  if (inputsOf(w1.source.lines, beforeSecond).some(isKey(KEYS.d.evdev, true))) stale.failures.push('a key typed into the newly focused window reached the one it replaced');

  // recovery: drop the first page's live plane while the second window streams.
  step('drop the live plane');
  if ((started.startUs + SECOND.seconds * 1e6 - nowUs()) / 1e6 < 10) throw new Invalid('the second window streamed too briefly to drop its stream under it');
  const lastBefore = await main.evaluate(lastDrawn(prefix2));
  const attemptsBefore = (await main.evaluate('globalThis.__tiloStream.transport')).attempts;
  const droppedAt = nowUs();
  recovery.droppedSessions = await main.evaluate('globalThis.__tiloDropLive()');
  if (!recovery.droppedSessions) throw new Invalid('the first page had no WebTransport session to drop');
  // Resumed: the page connected again (a new WebTransport session) and drew
  // a newer frame of the window on the canvas it opened for it again (the
  // live plane going down closes every window it carried).
  const resumed = await eventually(async () => {
    const [latest, transport] = await Promise.all([main.evaluate(lastDrawn(prefix2)), main.evaluate('globalThis.__tiloStream.transport')]);
    return transport.attempts > attemptsBefore && latest && latest.wallUs > droppedAt && latest.index > (lastBefore?.index ?? -1)
      && latest.epoch !== lastBefore?.epoch ? latest : null;
  }, 90_000, 200);
  recovery.lastBefore = lastBefore?.index ?? null;
  recovery.reconnects = (await main.evaluate('globalThis.__tiloStream.transport')).attempts - attemptsBefore;
  recovery.resumedMs = resumed ? Math.round((resumed.wallUs - droppedAt) / 1000) : null;
  if (!resumed) {
    recovery.failures.push(`the window did not come back within 90 s of its stream dropping (last frame before it: ${recovery.lastBefore}, `
      + `${recovery.reconnects} WebTransport sessions opened since)`);
  }

  step('the second window goes still');
  const stopped = JSON.parse(await w2.source.line(line => { try { return JSON.parse(line).event === 'stopped'; } catch { return false; } },
    (SECOND.seconds + 120) * 1000, 'the end of the second stream'));
  const sent = ctx.readSent(w2.log);
  if (!sent.some(frame => frame.width === SECOND.resizeTo.width) || !sent.some(frame => frame.width === SECOND.width)) {
    throw new Invalid(`the second session did not stream at both sizes: ${JSON.stringify([...new Set(sent.map(frame => `${frame.width}x${frame.height}`))])}`);
  }
  const live = JSON.parse(await main.evaluate(ctx.live)).live;
  if (live?.state !== 'live') recovery.failures.push(`the first page's live plane is ${live?.state} after the drop, not live`);
  for (const [name, page] of pages) {
    await settleOn(page, prefix2, stopped.lastIndex, 120_000);
    const judged = judgeStream({sent, draws: await page.evaluate(drawsOf(prefix2)), lastIndex: stopped.lastIndex});
    const found = {first: recovery, fallback, viewer}[name];
    found.secondWindow = {...judged, failures: undefined};
    found.failures.push(...judged.failures.map(failure => `the second window: ${failure}`));
    const size = judged.finalSize;
    if (size && (size.width !== SECOND.resizeTo.width || size.height !== SECOND.resizeTo.height)) {
      resize.failures.push(`the ${name} page ended on the second window at ${size.width}x${size.height}, not its new ${SECOND.resizeTo.width}x${SECOND.resizeTo.height}`);
    }
    if (judged.misSized) resize.failures.push(`the ${name} page drew ${judged.misSized} pictures of the resized window at the wrong size`);
  }

  // The page on the HTTPS stream drives the window; the viewer may not.
  step('drive from the fallback page, try from the viewer');
  const surface2 = (await main.evaluate(windowAt(prefix2)))?.key?.split(':')[1] ?? (await watcher.evaluate(windowAt(prefix2)))?.key?.split(':')[1];
  const publisher2 = w2.session.device;
  const ownerSees = await other.evaluate(windowAt(prefix2));
  if (!ownerSees) throw new Invalid('the second window is not on screen on the page on the HTTPS stream');
  if (!ownerSees.control) fallback.failures.push('the owner\'s page on the HTTPS stream was offered no control of the window');
  else {
    mark = lines2();
    await click(other, ownerSees);
    if (!await waitInput(w2, mark, isButton(LEFT_BUTTON, true), 30_000)) fallback.failures.push('a click from the page on the HTTPS stream did not reach the window');
  }
  await sleep(1000);
  const quiet1 = lines1(), quiet2 = lines2();
  const viewerSees = await watcher.evaluate(windowAt(prefix2));
  viewer.windowOnScreen = Boolean(viewerSees);
  if (viewerSees?.control) viewer.failures.push('the viewer was offered control of the window');
  if (viewerSees) { await click(watcher, viewerSees); await typeKey(watcher, KEYS.z); }
  const legacyInput = events => `globalThis.peer.legacyInput(${JSON.stringify(publisher2)}, ${JSON.stringify(surface2)}, ${JSON.stringify(events)}).then(Boolean, () => false)`;
  if (!surface2) throw new Invalid('the second window\'s surface is on no page');
  if (await watcher.evaluate(legacyInput([]))) viewer.failures.push('the host told the viewer it may drive the window');
  if (await watcher.evaluate(legacyInput([{PointerButton: {button: LEFT_BUTTON, pressed: true}}, {PointerButton: {button: LEFT_BUTTON, pressed: false}}]))) {
    viewer.failures.push('the host took input the viewer sent it directly');
  }
  await sleep(3000);
  const leaked = [...inputsOf(w1.source.lines, quiet1), ...inputsOf(w2.source.lines, quiet2)];
  if (leaked.length) viewer.failures.push(`${leaked.length} input events reached the windows while only the viewer gave input: ${JSON.stringify(leaked.slice(0, 3).map(input => input.value))}`);

  // stale: the second window closes.
  step('close the second window');
  const closing2 = lines2();
  w2.source.process.stdin.write('end\n');
  await w2.source.line(line => { try { return JSON.parse(line).event === 'ended'; } catch { return false; } }, 10_000, 'the second window closing');
  const gone = await eventually(async () => main.evaluate(`!${keyed(prefix2)}`), 60_000);
  if (!gone) stale.failures.push('the closed window stayed on the first page');
  await typeKey(main, KEYS.e);
  if (await main.evaluate(legacyInput([{Key: {keysym: KEYS.e.evdev, pressed: true, modifiers: 0}}, {Key: {keysym: KEYS.e.evdev, pressed: false, modifiers: 0}}]))) {
    stale.failures.push('the host took typing for a window that had closed');
  }
  await sleep(3000);
  const late = inputsOf(w2.source.lines, closing2).filter(input => keyOf(input) || input.value?.ClipboardText);
  if (late.length) stale.failures.push(`${late.length} key events reached the window after it closed`);
  const again = await eventually(async () => {
    const box = await main.evaluate(windowAt(prefix1));
    return box?.control ? box : null;
  }, 60_000);
  if (!again) throw new Invalid('the first window did not come back on screen after the second closed');
  mark = lines1();
  await click(main, again);
  await waitInput(w1, mark, isFocus(true), 5000);
  await typeKey(main, KEYS.f);
  if (!await waitInput(w1, mark, isKey(KEYS.f.evdev, false), 30_000)) stale.failures.push('once the second window closed, a click on the first and a key typed did not reach it');
  for (const [name, session] of [['first', w1], ['second', w2]]) {
    const stray = focusDiscipline(inputsOf(session.source.lines));
    if (stray.length) stale.failures.push(`${stray.length} keys or pastes reached the ${name} window while it did not hold the keyboard: ${stray.slice(0, 3).join(', ')}`);
  }
  const pageErrors = {};
  for (const [name, page] of pages) if (page.errors.length) pageErrors[name] = page.errors.slice(0, 3);
  return {cases, children: [w2], pageErrors};
}
