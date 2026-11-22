// A person in their own headless Chromium on the local tilo.sh, as a
// visitor arrives there: a fresh profile opens the site and lands in the
// Lounge as a new person. A site that lets people in by invitation only (as
// production's does) is entered as a newcomer enters it:
//
//   --invite LINK   they arrive through this invitation (stack.sh invite)
//                   rather than the bare site, and press Join on its card.
//
// Then, as asked:
//
//   --follow LINK   they follow an invitation link and press Join on its card;
//   --key NAME      they make an API key: `tilo keys create NAME` runs as a
//                   backend's terminal runs it, and the person approves the
//                   link it prints on the site's own /sign-in page, in this
//                   same browser, by pressing Allow;
//   --no-pages      the key they make on the api's grants page does not
//                   operate web pages in Browser: the box is unticked;
//   --window NAME --type TEXT
//                   after following the invitation, a window of the
//                   workspace another device hosts (a Linux app in the
//                   workspace's VM) appears on their shell, as a tile whose
//                   accessible name matches NAME (a case-blind pattern); they
//                   click it and type TEXT, then Enter, key by key as a
//                   keyboard sends it. report.window says when it appeared,
//                   how long each key took to change the window's picture
//                   (keyToFrameMs), and a picture of it is kept as
//                   window.png;
//   --page NAME --changes N
//                   after following the invitation, the shell tile whose
//                   accessible name matches NAME (the hosted Browser window)
//                   is the page of a window another device draws, and they
//                   watch it: a line `{"shell": …}` says when the page is drawn
//                   there and how much of the tile is content rather than
//                   background, and every N pictures that differ from the one
//                   before are kept as page-<n>.png and said in a line of their
//                   own, with the milliseconds since the last one. That is how
//                   a check holds the person to seeing the agent's work on
//                   their shell while it happens;
//   --answer FIFO   once the rest is done, the report is printed and they
//                   stay on the site, where the page holding their root shows
//                   what their agents' hosts ask them on its question sheet.
//                   Each line read from FIFO, `allow NAME` or `deny NAME`,
//                   waits for the next question, keeps the person's view of it
//                   as NAME-asked.png, presses Allow once or Deny on the sheet,
//                   and keeps NAME-answered.png once the sheet is gone; a line
//                   of {name, answer, sheet, askedMs, answeredMs} is appended
//                   to OUT_DIR/answers.jsonl. `pair WORKSPACE` makes a pairing
//                   link into that workspace on the host this page is in, the
//                   device that follows it approved here as an agent's link
//                   is, and appends {pair: {workspace, link, invite,
//                   expiresAt}}. `done` (or the FIFO's end) ends it.
//   --pair-link LINK
//                   this browser is the device that follows LINK: it pairs to
//                   the person the link names, holding none of their root, and
//                   is in the workspace it opens; with --answer it answers
//                   there, with its own device key, as a device of theirs does.
//
// usage: node person.mjs SITE SERVER OUT_DIR [--invite LINK] [--follow LINK] [--key NAME --tilo BIN --api URL [--no-pages]] [--answer FIFO] [--pair-link LINK]
//
// Prints one JSON line: {arrivedMs, joinedMs, workspace, followed, key, entered}.
// `arrivedMs` is a fresh visitor's entry: from navigating to the site to the
// page done arriving and a member of the Lounge, which the entry-speed work
// measures. Saves the page's console and a screenshot in OUT_DIR, and exits
// non-zero, saying what never happened, when a step did not. With `--key` it
// also keeps what the person reads there, in both appearances: the sign-in
// card they allow, the api's grants page, and the key that page makes.
import {spawn} from 'node:child_process';
import {appendFileSync, createReadStream, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {createInterface} from 'node:readline';
import {join} from 'node:path';
import {inflateSync} from 'node:zlib';
import {launch, sleep, stepLimit} from '../../../../tools/web/chrome.mjs';

const [site, server, out, ...rest] = process.argv.slice(2);
const option = name => {
  const at = rest.indexOf(name);
  return at < 0 ? null : rest[at + 1];
};
if (!site || !server || !out) {
  console.error('usage: node person.mjs SITE SERVER OUT_DIR [--follow LINK] [--key NAME --tilo BIN --api URL [--no-pages]]');
  process.exit(2);
}
const invite = option('--invite');
const follow = option('--follow');
const keyName = option('--key');
const noPages = rest.includes('--no-pages');
const tiloBin = option('--tilo');
const api = option('--api');
const windowName = option('--window');
const typed = option('--type');
const pageName = option('--page');
const changes = Number(option('--changes') ?? 1);
const answerFifo = option('--answer');
const pairLink = option('--pair-link');
mkdirSync(out, {recursive: true});

const report = {site, server};
// The stack is plain HTTP on this machine: no certificate to accept, as for
// any person's browser.
const browser = await launch();
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false})
  .catch(() => {});

const text = () => tab.evaluate(`(document.body?.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 600)`)
  .catch(() => '');
// What the person's page is being waited on for, set as each wait begins: a
// helper that stops a person who outlasts their steps says it, and the page it
// showed, instead of leaving the stop unexplained.
let waiting = 'the wait to begin';
let finishing = false;
const finish = async (code, message) => {
  if (finishing) return;
  finishing = true;
  if (message) report.error = message;
  writeFileSync(join(out, 'console.log'), tab.log.join('\n'));
  // What the person gave up on goes out before the browser stops: a helper
  // waiting on them hears it, and a close that takes a while does not hold it
  // back.
  if (message) {
    const said = tab.log.slice(-30);
    if (said.length) console.error(`--- the page said:\n${said.join('\n')}`);
    console.error(`FAIL: ${message}`);
  }
  // With --answer the report was printed before the person stayed.
  if (!reported) console.log(JSON.stringify(report));
  const shot = await tab.screenshot().catch(() => null);
  if (shot) writeFileSync(join(out, 'page.png'), shot);
  await browser.close().catch(() => {});
  process.exit(code);
};
process.on('SIGTERM', async () => {
  const page = Promise.race([text(), sleep(stepLimit(1)).then(() => '(the page did not answer)')]);
  await finish(1, `the helper stopped the person while they waited for ${waiting}: ${await page}`);
});
let reported = false;
const until = async (expression, what, timeout = stepLimit(36)) => {
  waiting = what;
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) await finish(1, `${what} never happened in ${timeout / 1000} s: ${await text()}`);
    await sleep(100);
  }
};
// The page as a person would look at it, kept in OUT_DIR as `name-light.png`
// and `name-dark.png`: what each surface the check visits says, in the two
// appearances it has.
const capture = async name => {
  for (const theme of ['light', 'dark']) {
    await tab.send('Emulation.setEmulatedMedia', {
      media: 'screen',
      features: [{name: 'prefers-color-scheme', value: theme}],
    }).catch(() => {});
    await sleep(150);
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(join(out, `${name}-${theme}.png`), shot);
  }
  await tab.send('Emulation.setEmulatedMedia', {features: []}).catch(() => {});
};
const memberships = `JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}')`;
const entered = `JSON.parse(globalThis.localStorage.getItem('tilo-entered-workspaces') ?? '[]')`;

// A fresh visitor's entry, or a browser a person paired to themselves: the
// pairing link makes this browser a device of theirs, holding none of their
// root, in the workspace the link names.
const began = Date.now();
let membership = null;
if (pairLink) {
  await tab.navigate(pairLink);
  await until(`document.documentElement.dataset.entry === 'open'`, 'the page pairing this browser');
  // The page says whose device this becomes before it asks to be let in; the
  // person paired it to themselves, and goes on.
  membership = JSON.parse(await until(`(() => {
    const go = document.querySelector('[data-card="pair-whose"] .tilo-entry-primary');
    if (go && !go.dataset.pressed) { go.dataset.pressed = '1'; go.click(); }
    const now = ${memberships};
    const found = (Array.isArray(now) ? now : Object.values(now)).find(entry => entry?.paired && entry?.workspace);
    return found ? JSON.stringify(found) : '';
  })()`, 'this browser being paired to the person'));
  report.pairedMs = Date.now() - began;
  report.paired = membership;
  report.workspace = membership.workspace;
  console.error(`ok: the browser paired to ${membership.userPublicKey} and was in ${membership.workspace} after ${report.pairedMs} ms`);
} else {
  await tab.navigate(invite ?? site);
  // Through an invitation, its card names the workspace and offers Join:
  // pressed as the newcomer presses it, and the page goes on arriving.
  await until(`(() => {
    if (['done', 'stopped'].includes(document.documentElement.dataset.arrival)) return true;
    const join = ${JSON.stringify(Boolean(invite))}
      && [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
    if (join && !join.disabled && !join.dataset.pressed) { join.dataset.pressed = '1'; join.click(); }
    return false;
  })()`, invite ? 'the page arriving through the invitation' : 'the page arriving');
  if (await tab.evaluate('document.documentElement.dataset.arrival') !== 'done') {
    await finish(1, `the page stopped arriving: ${await text()}`);
  }
  report.arrivedMs = Date.now() - began;
  // The page keeps the membership under the origin it joined (`keepJoinedHost`): the site's own
  // when a link carried the invitation to a site that names no other server, the server's when it
  // did, one an invitation named for a server reached over iroh.
  const siteOrigin = new URL(site).origin;
  membership = JSON.parse(await until(`(() => {
    const kept = ${memberships}[${JSON.stringify(new URL(server).origin)}]
      ?? ${memberships}[${JSON.stringify(siteOrigin)}]
      ?? Object.values(${memberships}).find(entry => entry?.workspace);
    return kept?.workspace ? JSON.stringify(kept) : '';
  })()`, 'joining the Lounge'));
  report.joinedMs = Date.now() - began;
  report.workspace = membership.workspace;
  console.error(`ok: a fresh visitor arrived in ${report.arrivedMs} ms and was in the Lounge (${report.workspace}) at ${report.joinedMs} ms`);
}

if (follow) {
  const before = JSON.stringify(await tab.evaluate(`JSON.stringify(${entered})`));
  await tab.navigate(follow);
  // The invitation's card names the workspace and the role; they press Join.
  await until(`(() => {
    const join = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Join');
    if (!join || join.disabled) return false;
    join.click();
    return true;
  })()`, 'the invitation offering to join');
  // Joined: the page entered a workspace it had not, other than the Lounge.
  report.followed = await until(`(() => {
    const now = ${entered};
    const found = (Array.isArray(now) ? now : Object.values(now)).find(entry =>
      (entry?.workspace ?? entry?.scope) && (entry.workspace ?? entry.scope) !== ${JSON.stringify(report.workspace)});
    return found ? JSON.stringify(found) : '';
  })()`, `joining the workspace the invitation names (entered before: ${before})`).then(JSON.parse);
  console.error(`ok: the invitation landed them in ${JSON.stringify(report.followed)}`);
}

// The keys a keyboard sends for each character typed: its DOM code, and
// whether Shift is held. What the page hands the hosted window is the key, not
// the text, so each character goes as the key that makes it.
const keyOf = char => {
  if (/^[a-z]$/.test(char)) return {code: `Key${char.toUpperCase()}`, key: char};
  if (/^[A-Z]$/.test(char)) return {code: `Key${char}`, key: char, shift: true};
  if (/^[0-9]$/.test(char)) return {code: `Digit${char}`, key: char};
  const named = {
    ' ': {code: 'Space', key: ' '}, '-': {code: 'Minus', key: '-'}, '.': {code: 'Period', key: '.'},
    '/': {code: 'Slash', key: '/'}, '>': {code: 'Period', key: '>', shift: true},
    '_': {code: 'Minus', key: '_', shift: true}, '=': {code: 'Equal', key: '='},
    '+': {code: 'Equal', key: '+', shift: true}, '$': {code: 'Digit4', key: '$', shift: true},
    '%': {code: 'Digit5', key: '%', shift: true}, '(': {code: 'Digit9', key: '(', shift: true},
    ')': {code: 'Digit0', key: ')', shift: true},
  };
  return named[char] ?? null;
};
const press = async ({code, key, shift = false}, text = key) => {
  const modifiers = shift ? 8 : 0;
  if (shift) await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', code: 'ShiftLeft', key: 'Shift', windowsVirtualKeyCode: 16});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', code, key, text, modifiers});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', code, key, modifiers});
  if (shift) await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', code: 'ShiftLeft', key: 'Shift', windowsVirtualKeyCode: 16});
};

// A picture of a window's box, as the PNG the browser writes it: what the
// person looking at their shell sees of it. The pixels are read back so that
// two pictures can be compared by what changed in them, not by whether the
// file's bytes differ — a caret blinking is not the page changing.
const pixelsOf = png => {
  if (png.length < 24 || png.readUInt32BE(0) !== 0x89504e47) throw new Error('not a PNG');
  let at = 8;
  let width = 0;
  let height = 0;
  let depth = 0;
  let colour = 0;
  let interlace = 0;
  const parts = [];
  while (at + 8 <= png.length) {
    const length = png.readUInt32BE(at);
    const kind = png.toString('ascii', at + 4, at + 8);
    const body = png.subarray(at + 8, at + 8 + length);
    if (kind === 'IHDR') {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      depth = body[8];
      colour = body[9];
      interlace = body[12];
    } else if (kind === 'IDAT') {
      parts.push(body);
    } else if (kind === 'IEND') {
      break;
    }
    at += 12 + length;
  }
  if (depth !== 8 || interlace !== 0 || (colour !== 2 && colour !== 6)) {
    throw new Error(`a PNG this does not read: depth ${depth}, colour ${colour}, interlace ${interlace}`);
  }
  const channels = colour === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(parts));
  const stride = width * channels;
  const data = Buffer.alloc(width * height * 4, 255);
  let previous = Buffer.alloc(stride);
  for (let row = 0; row < height; row++) {
    const filter = raw[row * (stride + 1)];
    const current = Buffer.from(raw.subarray(row * (stride + 1) + 1, (row + 1) * (stride + 1)));
    for (let i = 0; i < stride; i++) {
      const left = i >= channels ? current[i - channels] : 0;
      const up = previous[i];
      const upLeft = i >= channels ? previous[i - channels] : 0;
      let predictor = 0;
      if (filter === 1) predictor = left;
      else if (filter === 2) predictor = up;
      else if (filter === 3) predictor = (left + up) >> 1;
      else if (filter === 4) {
        const p = left + up - upLeft;
        const pa = Math.abs(p - left);
        const pb = Math.abs(p - up);
        const pc = Math.abs(p - upLeft);
        predictor = pa <= pb && pa <= pc ? left : (pb <= pc ? up : upLeft);
      }
      current[i] = (current[i] + predictor) & 0xff;
    }
    for (let x = 0; x < width; x++) {
      const from = x * channels;
      const to = (row * width + x) * 4;
      data[to] = current[from];
      data[to + 1] = current[from + 1];
      data[to + 2] = current[from + 2];
      if (channels === 4) data[to + 3] = current[from + 3];
    }
    previous = current;
  }
  return {width, height, data};
};
// How many pixels two pictures of the same box differ in, by more than a
// colour change this compression and scaling could make on its own.
const differingPixels = (before, after) => {
  if (before.width !== after.width || before.height !== after.height) return before.width * before.height;
  let count = 0;
  for (let i = 0; i < before.data.length; i += 4) {
    if (Math.abs(before.data[i] - after.data[i]) > 16
      || Math.abs(before.data[i + 1] - after.data[i + 1]) > 16
      || Math.abs(before.data[i + 2] - after.data[i + 2]) > 16) count++;
  }
  return count;
};
// How much of a picture is what is drawn in it rather than the box's
// background: a page with a form on it is not a tile waiting for one.
const contentShare = picture => {
  const counts = new Map();
  for (let i = 0; i < picture.data.length; i += 4 * 7) {
    const colour = (picture.data[i] << 16) | (picture.data[i + 1] << 8) | picture.data[i + 2];
    counts.set(colour, (counts.get(colour) ?? 0) + 1);
  }
  const background = [...counts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0];
  let drawn = 0;
  let seen = 0;
  for (let i = 0; i < picture.data.length; i += 4) {
    seen++;
    const colour = (picture.data[i] << 16) | (picture.data[i + 1] << 8) | picture.data[i + 2];
    if (colour !== background) drawn++;
  }
  return seen ? drawn / seen : 0;
};

if (windowName || pageName) {
  if (!follow) await finish(2, '--window and --page need --follow');
  const name = windowName ?? pageName;
  const pattern = new RegExp(name, 'i');
  const waiting = Date.now();
  // The page's semantic layer names every tile: the hosted window's is found
  // by its name, as a screen reader finds it.
  await tab.send('Accessibility.enable', {}).catch(() => {});
  let found = null;
  while (!found && Date.now() - waiting < 180_000) {
    const {result} = await tab.send('Accessibility.getFullAXTree', {}).catch(() => ({result: {nodes: []}}));
    found = (result?.nodes ?? []).find(node => !node.ignored && node.backendDOMNodeId
      && pattern.test(String(node.name?.value ?? '')));
    if (!found) await sleep(250);
  }
  if (!found) await finish(1, `no window named like /${name}/i appeared on the shell in 180 s (it reads: ${await text()})`);
  report.window = {name: found.name?.value, role: found.role?.value, appearedMs: Date.now() - waiting};
  console.error(`ok: the window ${JSON.stringify(report.window.name)} is on the shell after ${report.window.appearedMs} ms`);
  const {result: box} = await tab.send('DOM.getBoxModel', {backendNodeId: found.backendDOMNodeId});
  const quad = box?.model?.content ?? [];
  if (quad.length < 8) await finish(1, `the window ${JSON.stringify(report.window.name)} has no box on the page to click`);
  const x = (quad[0] + quad[4]) / 2;
  const y = (quad[1] + quad[5]) / 2;
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  }
  report.window.clip = {x: quad[0], y: quad[1], width: quad[4] - quad[0], height: quad[5] - quad[1]};
}

if (windowName) {
  if (!follow || typed === null) await finish(2, '--window needs --follow and --type');
  const clip = {...report.window.clip, scale: 1};
  // A picture arrives once the window's stream is decoded: what the person
  // sees before they type.
  await sleep(1500);
  const before = await tab.screenshot().catch(() => null);
  if (before) writeFileSync(join(out, 'window-before.png'), before);
  const picture = async () => (await tab.send('Page.captureScreenshot', {format: 'png', clip})
    .catch(() => ({data: ''}))).data;
  const answered = [];
  const typing = Date.now();
  for (const char of typed) {
    const key = keyOf(char);
    if (!key) await finish(2, `--type has ${JSON.stringify(char)}, which no key here types`);
    const seen = await picture();
    const pressed = performance.now();
    await press(key);
    for (;;) {
      const elapsed = performance.now() - pressed;
      if (elapsed > 3000) { answered.push(null); break; }
      if (await picture() !== seen) { answered.push(Math.round(elapsed)); break; }
    }
  }
  await press({code: 'Enter', key: 'Enter'}, '\r');
  report.window.typedAt = typing;
  report.window.typedMs = Date.now() - typing;
  const timed = answered.filter(ms => ms !== null).sort((a, b) => a - b);
  const at = share => timed[Math.min(timed.length - 1, Math.floor(share * timed.length))];
  report.window.keyToFrameMs = {
    keys: answered.length,
    unanswered: answered.length - timed.length,
    median: timed.length ? at(0.5) : null,
    p90: timed.length ? at(0.9) : null,
    each: answered,
  };
  console.error(`ok: a key reaches the window's next picture in ${report.window.keyToFrameMs.median} ms (median), ${report.window.keyToFrameMs.p90} ms (90th percentile) over ${timed.length} of ${answered.length} keys`);
  await sleep(1500);
  const after = await tab.screenshot().catch(() => null);
  if (after) writeFileSync(join(out, 'window.png'), after);
}

if (pageName) {
  if (!follow) await finish(2, '--page needs --follow');
  const clip = {...report.window.clip, scale: 1};
  const shot = async () => (await tab.send('Page.captureScreenshot', {format: 'png', clip})
    .catch(() => ({data: ''}))).data;
  const look = async () => pixelsOf(Buffer.from(await shot(), 'base64'));
  // A picture arrives once the window's page is drawn there: the person
  // waits for one the publisher sent, as an arriving card is no page.
  let drawn = null;
  const lookFrom = Date.now();
  while (!drawn && Date.now() - lookFrom < 120_000) {
    const picture = await look().catch(() => null);
    if (picture?.width && contentShare(picture) >= 0.02) drawn = picture;
    else await sleep(250);
  }
  if (!drawn) await finish(1, `the page on the shell is still the arriving card after 120 s (it reads: ${await text()})`);
  writeFileSync(join(out, 'page-shell.png'), Buffer.from(await shot(), 'base64'));
  const said = {name: report.window.name, appearedMs: report.window.appearedMs,
    drawnMs: Date.now() - lookFrom, content: Number(contentShare(drawn).toFixed(4))};
  report.shell = said;
  // A line of its own, before the run ends: the check goes on to have the
  // agent work the page while the person watches, and reads these as they come.
  process.stdout.write(`${JSON.stringify({shell: said})}\n`);
  console.error(`ok: the page is drawn on the shell after ${said.drawnMs} ms, ${(said.content * 100).toFixed(1)}% of the tile its content`);
  // Every picture that differs from the one before is the page changing
  // under their eyes. A caret blinking moves a few pixels; the agent filling
  // a field or a page showing its receipt moves many.
  let previous = drawn;
  const watching = Date.now();
  let changed = 0;
  let at = watching;
  while (changed < changes && Date.now() - watching < 600_000) {
    // The check says when it has seen what it asked them to look at: the
    // browser goes with the page, as any closed one does.
    if (existsSync(join(out, 'stop'))) break;
    const picture = await look().catch(() => null);
    if (!picture) { await sleep(200); continue; }
    const moved = differingPixels(previous, picture);
    if (moved > 150) {
      previous = picture;
      const kept = await shot();
      changed++;
      writeFileSync(join(out, `page-${changed}.png`), Buffer.from(kept, 'base64'));
      const one = {n: changed, ms: Date.now() - at, moved, picture: join(out, `page-${changed}.png`)};
      at = Date.now();
      console.error(`ok: the page on the shell changed ${moved} pixels, ${one.ms} ms after the change before`);
      report.changes = [...(report.changes ?? []), one];
      process.stdout.write(`${JSON.stringify({change: one})}\n`);
      continue;
    }
    await sleep(150);
  }
  if (changed < changes && !existsSync(join(out, 'stop'))) {
    await finish(1, `the page on the shell changed ${changed} times of the ${changes} the check waits for`);
  }
}

if (keyName) {
  if (!tiloBin || !api) await finish(2, '--key needs --tilo and --api');
  // A backend's terminal: its own home, the local api.
  const home = join(out, 'terminal');
  mkdirSync(home, {recursive: true});
  const terminal = spawn(tiloBin, ['keys', 'create', keyName], {
    env: {...process.env, HOME: home, XDG_STATE_HOME: join(home, 'state'), XDG_CONFIG_HOME: join(home, 'config'),
      TILO_API_URL: api},
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let said = '';
  let printed = '';
  terminal.stderr.on('data', chunk => { said += chunk; });
  terminal.stdout.on('data', chunk => { printed += chunk; });
  const ended = new Promise(resolve => terminal.on('exit', resolve));
  waiting = 'tilo keys create to print the link';
  const deadline = Date.now() + stepLimit(6);
  let link = null;
  while (!link) {
    link = said.match(/^\s*(https?:\/\/\S+)\s*$/m)?.[1] ?? null;
    if (!link && Date.now() > deadline) await finish(1, `tilo keys create printed no link: ${said}`);
    if (!link) await sleep(100);
  }
  report.approveLink = link;
  await tab.navigate(link);
  await until(`['asked', 'stopped'].includes(document.documentElement.dataset.signIn)`, 'the sign-in page asking');
  if (await tab.evaluate('document.documentElement.dataset.signIn') !== 'asked') {
    await finish(1, `the sign-in page stopped: ${await text()}`);
  }
  report.asked = await text();
  // The card the person reads before they allow: what the key reaches, and
  // what it may do in Browser.
  await capture('sign-in');
  await tab.evaluate(`document.getElementById('sign-in-allow').click()`);
  waiting = 'tilo keys create to get the key';
  const code = await Promise.race([ended, sleep(stepLimit(24)).then(() => 'timeout')]);
  if (code !== 0) {
    terminal.kill();
    await finish(1, `tilo keys create did not get the key (${code}): ${said} | the page: ${await text()}`);
  }
  report.key = printed.match(/TILO_API_KEY=(\S+)/)?.[1] ?? null;
  if (!report.key) await finish(1, `tilo keys create printed no key: ${printed}`);
  console.error(`ok: they approved "${keyName}" on the site's sign-in page and the terminal got its key`);

  // The grants page the same browser is now signed in to: the keys, each
  // saying whether it operates web pages in Browser, and the way to revoke
  // one; and the page that shows the key it makes once.
  await tab.navigate(`${api}/grants`);
  await until(`document.querySelector('ul.grants, form.new') !== null`, 'the grants page');
  await capture('grants');
  await tab.evaluate(`(() => {
    document.querySelector('form.new input[name="name"]').value = 'The grants page key';
    // What the person gives this key: a key they do not let operate web
    // pages asks them about each page action that acts on one.
    const box = document.querySelector('form.new input[name="browser_pages"]');
    if (box) box.checked = ${noPages ? 'false' : 'true'};
    document.querySelector('form.new').requestSubmit();
    return true;
  })()`);
  // The api asks the person to allow the key it would make (the same card
  // the site showed for the terminal's key); allowing it answers with the
  // key, which that page shows once.
  const making = await until(`document.querySelector('code.key') ? 'made'
    : (['asked', 'stopped'].includes(document.documentElement.dataset.signIn) ? 'asked' : '')`,
  'the card asking for the key or the key itself');
  if (making === 'asked') {
    await capture('key-ask');
    await tab.evaluate(`document.getElementById('sign-in-allow').click()`);
  }
  await until(`document.querySelector('code.key') !== null`, 'the key the grants page made');
  report.madeOnPage = await text();
  report.madeKey = await tab.evaluate(`document.querySelector('code.key').textContent.trim()`);
  if (noPages) {
    report.noPagesKey = report.madeKey;
    report.noPagesSaid = report.madeOnPage;
  }
  await capture('key');
}
// The page's semantic layer names every control it draws: the first node
// whose accessible name matches `pattern` (and whose role is `role`, if
// named), as a screen reader finds it, or null after `within` ms.
const axNamed = async (pattern, {role = null, within = 180_000} = {}) => {
  await tab.send('Accessibility.enable', {}).catch(() => {});
  const waiting = Date.now();
  for (;;) {
    const {result} = await tab.send('Accessibility.getFullAXTree', {}).catch(() => ({result: {nodes: []}}));
    const found = (result?.nodes ?? []).find(node => !node.ignored && node.backendDOMNodeId
      && pattern.test(String(node.name?.value ?? '')) && (!role || node.role?.value === role));
    if (found || Date.now() - waiting > within) return found ?? null;
    await sleep(250);
  }
};
// A click at the middle of `node`'s box, as a mouse gives it; false when it
// has no box on the page.
const clickNode = async node => {
  const {result: box} = await tab.send('DOM.getBoxModel', {backendNodeId: node.backendDOMNodeId});
  const quad = box?.model?.content ?? [];
  if (quad.length < 8) return false;
  const x = (quad[0] + quad[4]) / 2;
  const y = (quad[1] + quad[5]) / 2;
  for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  }
  return true;
};

if (answerFifo) {
  // The report now: the caller reads it while the person stays.
  console.log(JSON.stringify(report));
  reported = true;
  // Back on the site, where the page holding their root answers what their
  // agents' hosts ask them (entry-page.mjs, answerAgentQuestions). A browser
  // paired to them stays where it is, in the workspace the pairing put it in.
  if (pairLink) {
    console.error('ok: the paired browser is in its workspace, answering the person\'s questions there');
  } else {
    await tab.navigate(site);
    await until(`document.documentElement.dataset.arrival === 'done'`, 'the page arriving again to answer questions');
    console.error('ok: the person is on the site, answering their agents\' questions');
  }
  const asks = createInterface({input: createReadStream(answerFifo)});
  for await (const line of asks) {
    const [answer, name] = line.trim().split(/\s+/);
    if (!answer) continue;
    if (answer === 'done') break;
    if (answer === 'pair') {
      // A pairing link for a browser of the person's, into `name`'s
      // workspace: the device that follows it is approved here, as an agent
      // set up from this tab is.
      await until('Boolean(globalThis.tiloIdentity?.connectAgent)', 'the page ready to pair a device', 120_000);
      const made = await tab.evaluate(`(async () => {
        try {
          const made = await globalThis.tiloIdentity.connectAgent({workspace: ${JSON.stringify(name)}});
          return {link: made.link, invite: made.invite, expiresAt: made.expiresAt};
        } catch (error) {
          return {error: String(error?.message ?? error), code: error?.code ?? null};
        }
      })()`).catch(error => ({error: String(error?.message ?? error)}));
      appendFileSync(join(out, 'answers.jsonl'), `${JSON.stringify({pair: {workspace: name, ...made}})}\n`);
      if (made.link) console.error(`ok: the page made a pairing link into ${name}`);
      else console.error(`FAIL: the page made no pairing link into ${name}: ${JSON.stringify(made)}`);
      continue;
    }
    if (!['allow', 'deny'].includes(answer) || !name) await finish(2, `--answer reads "allow NAME", "deny NAME" or "pair WORKSPACE", not ${JSON.stringify(line)}`);
    const waiting = Date.now();
    const button = await axNamed(answer === 'allow' ? /^Allow once$/ : /^Deny$/, {within: 300_000});
    if (!button) await finish(1, `no question came to the person's sheet for ${name} in 300 s (the page reads: ${await text()})`);
    const askedMs = Date.now() - waiting;
    // The sheet as the person reads it, before they answer.
    await sleep(400);
    const sheet = await text();
    const asked = await tab.screenshot().catch(() => null);
    if (asked) writeFileSync(join(out, `${name}-asked.png`), asked);
    if (!await clickNode(button)) await finish(1, `the sheet's ${answer} button for ${name} has no box to press`);
    const pressed = Date.now();
    const gone = await (async () => {
      while (Date.now() - pressed < 30_000) {
        if (!await axNamed(/^(Allow once|Deny)$/, {within: 0})) return true;
        await sleep(200);
      }
      return false;
    })();
    if (!gone) await finish(1, `the sheet stayed up after the person pressed ${answer} for ${name}`);
    const answeredMs = Date.now() - pressed;
    await sleep(600);
    const after = await tab.screenshot().catch(() => null);
    if (after) writeFileSync(join(out, `${name}-answered.png`), after);
    appendFileSync(join(out, 'answers.jsonl'), `${JSON.stringify({name, answer, sheet, askedMs, answeredMs})}\n`);
    console.error(`ok: the person saw the question for ${name} after ${askedMs} ms and pressed ${answer}`);
  }
}
await finish(0);
