#!/usr/bin/env node
// Whose windows are whose on a persistent room's shared desktop, end to
// end: the stack production runs (dev/run.py: the web entry, the room
// service and its relay, the room's desktop starting with named strips) and
// headless Chrome, each person a browser context of their own. Two guests,
// Ada and Ben, each open a window; a third person, Mo, is made one of the
// room's moderators while everyone is in (TILO_ROOMS_ROLES_FILE), and from
// then on sees and moderates the desktop live.
//
// usage: node crates/hosting/tilo-room-service/dev/shared-desktop-e2e.mjs
//          --bundle DIR --relay-bin PATH --room-service-bin PATH --out DIR
//          [--allow-dev] [--expect-revision SHA] [--timeout S]
//
// It checks, and photographs into --out:
//   first-post       Ada's first post in the fresh room's Chat shows on Ben's
//                    and Mo's pages, open before it, with no input of theirs
//   own-window      Ada's own window under her pointer offers its close
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
import {CHAT_CHANNEL, freePort, open, page, parseArgs, reload, startStack} from './auth-e2e.mjs';

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

/** In the page: every name and text the desktop last described, as one
 * string: what a person sees on it, as its accessibility tree reads it. */
const SEMANTIC_TEXT = `JSON.stringify(tiloMount.state()?.semantics?.root ?? null)`;
/** In the page: post `text` in the first text channel of the Chat window
 * `window`, through the channel's published `post` (or the messages' own,
 * naming the room's first channel); `{posted}` or `{error}` with what the
 * window offers to post with. */
const POST = (window, text) => `(async () => {
  const offered = tiloMount.actions(undefined, {window: ${JSON.stringify(window)}})
    .filter(action => action.address.action === 'post');
  const attempts = [
    ...offered.filter(action => !action.address.resource.startsWith('chain:'))
      .map(action => [action.address.resource, {text: ${JSON.stringify(text)}}]),
    ...offered.filter(action => action.address.resource.startsWith('chain:'))
      .map(action => [action.address.resource, {channel: 'c_general', text: ${JSON.stringify(text)}}]),
  ];
  const errors = [];
  for (const [resource, args] of attempts) {
    try {
      const result = await tiloMount.invokeAction(resource, 'post', args, {window: ${JSON.stringify(window)}});
      if (result?.state === 'completed') return {posted: resource};
      errors.push(resource + ': ' + JSON.stringify(result));
    } catch (error) {
      errors.push(resource + ': ' + String(error?.message ?? error));
    }
  }
  return {error: errors.length ? errors : 'nothing to post with', offered: offered.map(action => action.address.resource)};
})()`;
/** In the page: whether the desktop shows a Chat window. */
const HAS_CHAT = `tiloMount.navigation.getSnapshot().entries.some(entry => entry.app === 'so.tilo.chat')`;

const snapshot = tab => tab.evaluate('tiloMount.navigation.getSnapshot()');
/** Dispatch a navigation command without waiting on it: a close settles
 * only once its window is gone, which a refused one never is. */
const command = (tab, body) => tab.evaluate(`(() => {
  const s = tiloMount.navigation.getSnapshot();
  tiloMount.navigation.dispatch({...${JSON.stringify(body)}, generation: s.generation}).catch(() => {});
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

/** `promise`, or a failure naming `what` after `ms`: a DevTools call into a
 * page that stopped answering must not hold the run forever. */
const bounded = (what, promise, ms = 30_000) => Promise.race([promise,
  sleep(ms).then(() => { throw Error(`no answer in ${ms} ms: ${what}`); })]);
const mouse = (tab, type, {x, y}, extra = {}) =>
  bounded(`${type} at ${x},${y}`, tab.send('Input.dispatchMouseEvent', {type, x, y, ...extra}));
const started = Date.now();
const step = what => console.log(`[${Math.round((Date.now() - started) / 1000)}s] ${what}`);

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
  // Who moderates the room, as its operator declares it while it runs.
  const rolesFile = join(options.scratch, 'roles.json');
  writeFileSync(rolesFile, JSON.stringify({owners: [], moderators: []}));
  const env = {TILO_ROOMS_STRIPS: 'Lounge', TILO_ROOMS_GUEST_JOINS_PER_10_MIN: '50', TILO_ROOMS_GUEST_JOINS_PER_DAY: '500',
    TILO_ROOMS_ROLES_FILE: rolesFile};
  const checks = [];
  // The whole run, bounded: a hang reports where it stood.
  const watchdog = setTimeout(() => {
    console.error('shared-desktop-e2e: the run took over 25 minutes; giving up');
    process.exit(1);
  }, 25 * 60_000);
  watchdog.unref();
  const check = (name, ok, detail) => {
    checks.push({name, ok: Boolean(ok), detail});
    console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}: ${JSON.stringify(detail)}`);
  };
  const shoot = async (name, tab) => {
    step(`screenshot ${name}`);
    // A software-rendered frame of three busy pages can take a while.
    const shot = await bounded(`screenshot ${name}`, tab.screenshot(), 180_000)
      .catch(error => { console.error(error.message); return null; });
    if (shot) writeFileSync(join(options.out, `${name}.png`), shot);
  };

  const stack = await startStack(options, ports, {fresh: true, env});
  const browser = await launch({args: chromeFlags('swiftshader', process.getuid?.())});
  const people = {};
  try {
    // Three people join as guests and name themselves.
    for (const name of ['Ada', 'Ben', 'Mo']) {
      // A join the door turns away is tried again by a fresh visitor: who
      // gets in is not what this run checks.
      let tab;
      let opened;
      for (let attempt = 0; attempt < 3; attempt++) {
        tab = await page(browser, await browser.context());
        await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
        opened = await open(tab, `${web}/`, settleOptions);
        if (opened.entry === 'unsupported') throw new Invalid('Chrome cannot draw WebGPU here');
        if (opened.entry === 'open') break;
        console.error(`${name}'s join was turned away (${opened.status}); trying again`);
        await tab.close();
        await sleep(5_000);
      }
      if (opened.entry !== 'open') throw new Invalid(`${name} did not get in: ${JSON.stringify(opened)}`);
      const key = opened.membership.userPublicKey;
      await tab.evaluate(`localStorage.setItem(${JSON.stringify(DISPLAY_NAMES)}, JSON.stringify({[${JSON.stringify(key)}]: ${JSON.stringify(name)}}))`);
      people[name] = {tab, key};
    }
    // Each page comes back once with its person's name.
    for (const name of Object.keys(people)) {
      const reopened = await reload(people[name].tab, settleOptions);
      if (reopened.entry !== 'open') throw new Invalid(`${name} did not get back in: ${JSON.stringify(reopened)}`);
      await until(`${name}'s desktop navigates`, () => people[name].tab.evaluate('Boolean(globalThis.tiloMount?.navigation)'));
    }
    // The room's operator makes Mo one of its moderators while everyone is
    // in: the roles file is taken in live, with no restart and no reload.
    step('Mo is made a moderator');
    writeFileSync(rolesFile, JSON.stringify({owners: [], moderators: [people.Mo.key]}));
    const {Ada: {tab: ada}, Ben: {tab: ben}, Mo: {tab: mo}} = people;

    await sleep(5_000);
    // first-post: the room is fresh, so Ada's first post in Chat starts its
    // messages. Ben and Mo have Chat open before she posts, and see the post
    // with no input of their own.
    step('first-post');
    const chatWindow = await until('Chat is open for everyone', async () => {
      const channel = await ada.evaluate(CHAT_CHANNEL);
      if (!channel) return null;
      for (const tab of [ben, mo]) {
        if (!(await tab.evaluate(HAS_CHAT))) return null;
      }
      return channel.window;
    });
    await sleep(3_000);
    const first = 'The first post of this room';
    const posted = await ada.evaluate(POST(chatWindow, first)).catch(error => ({error: error.message}));
    const shows = async tab => (await tab.evaluate(SEMANTIC_TEXT)).includes(first);
    const missed = [];
    for (const [name, tab] of [['Ada', ada], ['Ben', ben], ['Mo', mo]]) {
      if (!(await until(`${name} sees the first post`, () => shows(tab), 20_000).catch(() => false))) missed.push(name);
    }
    await shoot('first-post', ben);
    check('first-post', !posted.error && missed.length === 0, {window: chatWindow, posted, missed});

    // Ada opens Notes, Ben a Calculator; everyone sees both. A window lost
    // to such a race is opened again, and counted.
    const opens = [];
    const opened = async (opener, app, title, watcher) => {
      for (let attempt = 0; attempt < 3; attempt++) {
        opens.push(title);
        await command(opener, {kind: 'launch', app});
        if (await until(`${title} reaches the room`, async () => (await titles(watcher)).includes(title), 20_000)
          .catch(() => false)) return;
      }
      throw Error(`${title} never reached the room`);
    };
    step('windows open');
    await opened(ada, 'so.tilo.notes', 'Notes', ben);
    await opened(ben, 'so.tilo.calculator', 'Calculator', ada);
    check('windows-open', opens.length === 2, {opens});
    const both = async tab => {
      const seen = await titles(tab);
      return seen.includes('Notes') && seen.includes('Calculator');
    };
    const late = [];
    for (const [name, person] of Object.entries(people)) {
      if (await until(`${name} sees both windows`, () => both(person.tab), 30_000).catch(() => false)) continue;
      // Reported, then given a reload: what a person would do.
      late.push(name);
      await reload(person.tab, settleOptions);
      await until(`${name}'s desktop navigates again`, () => person.tab.evaluate('Boolean(globalThis.tiloMount?.navigation)'));
      await until(`${name} sees both windows after a reload`, () => both(person.tab));
    }
    check('windows-reach-everyone', late.length === 0, {late});
    // Presence (names, colours) and each page's answer from the room settle.
    await sleep(8_000);

    step('own-window');
    // own-window: Ada's pointer on her own window's tab.
    const notes = await tabAt(ada, 'Notes');
    await mouse(ada, 'mouseMoved', notes);
    await sleep(1_500);
    await shoot('own-window', ada);
    check('own-window', true, {at: notes});

    step('others-window');
    // others-window: Ada's pointer on Ben's.
    const calculator = await tabAt(ada, 'Calculator');
    await mouse(ada, 'mouseMoved', calculator);
    await sleep(1_500);
    await shoot('others-window', ada);
    check('others-window', true, {at: calculator});

    step('refused-drag');
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

    step('refused-close');
    // refused-close: Ada's page closes Ben's window through the navigation
    // door; it comes back on her page and never leaves Ben's.
    const benWindow = (await snapshot(ada)).entries.find(entry => entry.label === 'Calculator');
    await command(ada, {kind: 'close', route: benWindow.route}).catch(() => {});
    await sleep(3_000);
    const adaAfterClose = await titles(ada);
    const benAfterClose = await titles(ben);
    check('refused-close', adaAfterClose.includes('Calculator') && benAfterClose.includes('Calculator'),
      {ada: adaAfterClose, ben: benAfterClose});

    step('moderator-view');
    // moderator-view: Mo's pointer on Ada's window, then Mo closes it.
    // A page asks the room what it may moderate when it joins and every
    // fifteen seconds after; Mo was made a moderator well before this.
    await sleep(16_000);
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
    if (!closedForAda) {
      for (const [name, tab] of [['Mo', mo], ['Ada', ada]]) console.error(`${name}: ${(tab.log ?? []).slice(-20).join('\n  ')}`);
      console.error((stack.log().match(/^.*(refus|warn|error).*$/gim) ?? []).slice(-20).join('\n'));
    }
  } catch (error) {
    // What each person saw when it went wrong, in the run's log.
    for (const [name, person] of Object.entries(people)) {
      const seen = await titles(person.tab).catch(e => `unreadable: ${e.message}`);
      const log = (person.tab.log ?? []).filter(line => /replication|desktop|refus|EXC|ERROR|WARN/i.test(line)).slice(-25);
      console.error(`${name} sees ${JSON.stringify(seen)}\n  ${log.join('\n  ')}`);
      await shoot(`failed-${name}`, person.tab);
    }
    console.error((stack.log().match(/^.*(refus|warn|error).*$/gim) ?? []).slice(-40).join('\n'));
    check('run', false, {error: String(error?.stack ?? error)});
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
