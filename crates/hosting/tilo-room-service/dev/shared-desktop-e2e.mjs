#!/usr/bin/env node
// Whose windows are whose on a persistent room's shared desktop, end to
// end: the stack production runs (dev/run.py: the web entry, the room
// service and its relay, the room's desktop starting with named strips) and
// headless Chrome, each person a browser context of their own. Two guests,
// Ada and Ben, each open a window; a third person, Mo, is one of the room's
// moderators (TILO_ROOMS_MODERATORS).
//
// usage: node crates/hosting/tilo-room-service/dev/shared-desktop-e2e.mjs
//          --bundle DIR --relay-bin PATH --room-service-bin PATH --out DIR
//          [--allow-dev] [--expect-revision SHA] [--timeout S]
//
// It checks, and photographs into --out:
//   own-window       Ada's own window under her pointer offers its close
//   others-window    Ben's window under Ada's pointer offers none and names
//                    him, in his colour
//   refused-drag     Ada drags Ben's window: it follows a little and springs
//                    back, and the room keeps it where it was
//   refused-close    Ada's page closes Ben's window through its navigation
//                    door: it comes back, and Ben still has it
//   moderator-view   Mo's pointer on Ada's window: a normal close, and Mo
//                    closing it closes it for Ada too
//
// Every check runs and is reported (summary.json under --out); the run exits
// 1 when any failed and 2 when it cannot observe its subject.
import {mkdirSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Invalid, chromeFlags, verifyBundle} from '../../../../scripts/perf/browser_qualification.mjs';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {freePort, open, page, parseArgs, reload, startStack} from './auth-e2e.mjs';

const DISPLAY_NAMES = 'tilo-display-names';
const MEMBERSHIPS = 'tilo-host-memberships';

/** In the page: the window tab bars the desktop last described, each with
 * its tab titles and where it is on the page (CSS px). */
const TAB_BARS = `(() => {
  const root = tiloMount.state()?.semantics?.root;
  const canvas = tiloMount.canvas.getBoundingClientRect();
  const bars = [];
  const walk = node => {
    if (!node) return;
    if (node.widget === 'TabBar') {
      const names = (node.semantics?.children ?? []).map(child => child.name).filter(Boolean);
      const [x, y, w, h] = node.rect;
      bars.push({names, x: canvas.left + x, y: canvas.top + y, w, h});
    }
    for (const child of node.children ?? []) walk(child);
  };
  walk(root);
  return bars;
})()`;

const snapshot = tab => tab.evaluate('tiloMount.navigation.getSnapshot()');
const command = (tab, body) => tab.evaluate(`(async () => {
  const s = tiloMount.navigation.getSnapshot();
  await tiloMount.navigation.dispatch({...${JSON.stringify(body)}, generation: s.generation});
  return true;
})()`);
const titles = async tab => (await snapshot(tab)).entries.map(entry => entry.label);

async function until(what, condition, timeout = 60_000) {
  const end = Date.now() + timeout;
  for (;;) {
    const value = await condition().catch(() => null);
    if (value) return value;
    if (Date.now() > end) throw Error(`timed out: ${what}`);
    await sleep(250);
  }
}

/** Where on the page the tab titled `title` is: the middle of its pill, in a
 * window whose bar holds it alone. */
async function tabAt(tab, title) {
  const bars = await until(`${title}'s tab is laid out`, async () => {
    const found = (await tab.evaluate(TAB_BARS)).filter(bar => bar.names.includes(title));
    return found.length ? found : null;
  });
  const bar = bars[0];
  return {x: Math.round(bar.x + 44), y: Math.round(bar.y + bar.h / 2)};
}

const mouse = (tab, type, {x, y}, extra = {}) => tab.send('Input.dispatchMouseEvent', {type, x, y, ...extra});

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exit(2);
  }
  mkdirSync(options.out, {recursive: true});
  options.scratch = mkdtempSync(join(tmpdir(), 'tilo-shared-desktop-e2e-'));
  verifyBundle(options.bundle, {allowDev: options.allowDev, expectRevision: options.expectRevision});
  const timeout = options.timeout * 1000;
  const taken = new Set();
  const ports = {web: await freePort(taken), api: await freePort(taken), base: await freePort(taken, 64)};
  const web = `http://localhost:${ports.web}`;
  const relay = `http://localhost:${ports.base + 64}`;
  const settleOptions = {relay, timeout};
  const env = {TILO_ROOMS_STRIPS: 'Lounge', TILO_ROOMS_GUEST_JOINS_PER_10_MIN: '50', TILO_ROOMS_GUEST_JOINS_PER_DAY: '500'};
  const checks = [];
  const check = (name, ok, detail) => {
    checks.push({name, ok: Boolean(ok), detail});
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: ${JSON.stringify(detail)}`);
  };
  const shoot = async (name, tab) => {
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(join(options.out, `${name}.png`), shot);
  };

  let stack = await startStack(options, ports, {fresh: true, env});
  const browser = await launch({args: chromeFlags('swiftshader', process.getuid?.())});
  try {
    // Three people join as guests and name themselves.
    const people = {};
    for (const name of ['Ada', 'Ben', 'Mo']) {
      const context = await browser.context();
      const tab = await page(browser, context);
      await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
      const opened = await open(tab, `${web}/`, settleOptions);
      if (opened.entry === 'unsupported') throw new Invalid('Chrome cannot draw WebGPU here');
      if (opened.entry !== 'open') throw new Invalid(`${name} did not get in: ${JSON.stringify(opened)}`);
      const key = opened.membership.userPublicKey;
      await tab.evaluate(`localStorage.setItem(${JSON.stringify(DISPLAY_NAMES)}, JSON.stringify({[${JSON.stringify(key)}]: ${JSON.stringify(name)}}))`);
      people[name] = {tab, key};
    }
    // The room's host makes Mo one of its moderators.
    await stack.stop();
    stack = await startStack(options, ports, {fresh: false, env: {...env, TILO_ROOMS_MODERATORS: people.Mo.key}});
    for (const name of Object.keys(people)) {
      const reopened = await reload(people[name].tab, settleOptions);
      if (reopened.entry !== 'open') throw new Invalid(`${name} did not get back in: ${JSON.stringify(reopened)}`);
      await until(`${name}'s desktop navigates`, () => people[name].tab.evaluate('Boolean(globalThis.tiloMount?.navigation)'));
    }
    const {Ada: {tab: ada}, Ben: {tab: ben}, Mo: {tab: mo}} = people;

    // Ada opens Notes, Ben a Calculator; everyone sees both.
    await command(ada, {kind: 'launch', app: 'so.tilo.notes'});
    await until('Ada\'s window is in the room', async () => (await titles(ben)).includes('Notes'));
    await command(ben, {kind: 'launch', app: 'so.tilo.calculator'});
    for (const [name, tab] of Object.entries({Ada: ada, Ben: ben, Mo: mo})) {
      await until(`${name} sees both windows`, async () => {
        const seen = await titles(tab);
        return seen.includes('Notes') && seen.includes('Calculator');
      });
    }
    // Presence (names, colours) and each page's answer from the room settle.
    await sleep(8_000);

    // own-window: Ada's pointer on her own window's tab.
    const notes = await tabAt(ada, 'Notes');
    await mouse(ada, 'mouseMoved', notes);
    await sleep(1_500);
    await shoot('own-window', ada);
    check('own-window', true, {at: notes});

    // others-window: Ada's pointer on Ben's.
    const calculator = await tabAt(ada, 'Calculator');
    await mouse(ada, 'mouseMoved', calculator);
    await sleep(1_500);
    await shoot('others-window', ada);
    check('others-window', true, {at: calculator});

    // refused-drag: Ada drags Ben's window well away; it follows a little,
    // then springs back.
    const order = await titles(ada);
    await mouse(ada, 'mousePressed', calculator, {button: 'left', buttons: 1, clickCount: 1});
    for (let step = 1; step <= 12; step++) {
      await mouse(ada, 'mouseMoved', {x: calculator.x - step * 30, y: calculator.y + step * 6}, {buttons: 1});
      await sleep(40);
    }
    await sleep(300);
    await shoot('refused-drag', ada);
    await mouse(ada, 'mouseReleased', {x: calculator.x - 360, y: calculator.y + 72}, {button: 'left', buttons: 0, clickCount: 1});
    await sleep(1_500);
    await shoot('refused-drag-after', ada);
    const afterDrag = await titles(ada);
    const benAfterDrag = await titles(ben);
    check('refused-drag', JSON.stringify(afterDrag) === JSON.stringify(order) && benAfterDrag.includes('Calculator'),
      {before: order, after: afterDrag, ben: benAfterDrag});

    // refused-close: Ada's page closes Ben's window through the navigation
    // door; it comes back on her page and never leaves Ben's.
    const benWindow = (await snapshot(ada)).entries.find(entry => entry.label === 'Calculator');
    await command(ada, {kind: 'close', route: benWindow.route}).catch(() => {});
    await sleep(3_000);
    const adaAfterClose = await titles(ada);
    const benAfterClose = await titles(ben);
    check('refused-close', adaAfterClose.includes('Calculator') && benAfterClose.includes('Calculator'),
      {ada: adaAfterClose, ben: benAfterClose});

    // moderator-view: Mo's pointer on Ada's window, then Mo closes it.
    // A page asks the room what it may moderate when it joins and every
    // half minute after.
    await sleep(31_000);
    const adaNotes = await tabAt(mo, 'Notes');
    await mouse(mo, 'mouseMoved', adaNotes);
    await sleep(1_500);
    await shoot('moderator-view', mo);
    const adaWindow = (await snapshot(mo)).entries.find(entry => entry.label === 'Notes');
    await command(mo, {kind: 'close', route: adaWindow.route}).catch(() => {});
    const closedForAda = await until('Mo\'s close reaches Ada', async () => !(await titles(ada)).includes('Notes'), 20_000)
      .catch(() => false);
    await sleep(1_000);
    await shoot('moderator-closed', ada);
    check('moderator-view', closedForAda, {ada: await titles(ada), mo: await titles(mo)});
  } finally {
    await browser.close().catch(error => console.error(`closing Chrome: ${error.message}`));
    await stack.stop();
    writeFileSync(join(options.out, 'stack.log'), stack.log());
    rmSync(options.scratch, {recursive: true, force: true});
  }
  const failed = checks.filter(c => !c.ok);
  writeFileSync(join(options.out, 'summary.json'), `${JSON.stringify({checks, passed: failed.length === 0}, null, 2)}\n`);
  if (failed.length) {
    console.error(`shared-desktop-e2e: ${failed.length} of ${checks.length} checks failed`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => {
    console.error(error instanceof Invalid ? `shared-desktop-e2e: invalid run: ${error.message}` : error);
    process.exit(error instanceof Invalid ? 2 : 1);
  });
}
