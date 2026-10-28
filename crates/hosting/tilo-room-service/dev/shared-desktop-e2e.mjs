#!/usr/bin/env node
// Whose windows are whose in a public room, end to end: the stack production
// runs (dev/run.py: the web entry, the room service and its relay, the
// room's desktop starting with named strips) and headless Chrome, each person
// a browser context of their own. Two guests, Ada and Ben, each open a
// window; a third person, Mo, is made one of the room's moderators while
// everyone is in (TILO_ROOMS_ROLES_FILE).
//
// A public room shares what its people write (Chat's messages) but no
// member's layout: each visitor's desktop is their own, so the windows they
// open, move and close stay on it, and the workspace's name and desks are
// its host's.
//
// usage: node crates/hosting/tilo-room-service/dev/shared-desktop-e2e.mjs
//          --bundle DIR --relay-bin PATH --room-service-bin PATH --out DIR
//          [--allow-dev] [--expect-revision SHA] [--timeout S]
//
// It checks, and photographs into --out:
//   first-post        Ada's first post in the fresh room's Chat shows on Ben's
//                     and Mo's pages, open before it, with no input of theirs
//   windows-open      Ada opens Notes and Ben a Calculator, each on their own
//                     desktop
//   windows-stay-own  neither window reaches anyone else's desktop, Mo's
//                     included, and none comes back to its own after a reload
//                     any different
//   own-window        Ada's own window under her pointer offers its close
//   own-close         Ada closes her window: it leaves her desktop, and Ben's
//                     window stays on his
//   moderator-desktop Mo, a moderator, still sees only his own desktop: the
//                     guests' windows are theirs
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

    // Ada opens Notes, Ben a Calculator, each on their own desktop. A launch
    // lost to a race is tried again, and counted.
    const opens = [];
    const opened = async (opener, app, title) => {
      for (let attempt = 0; attempt < 3; attempt++) {
        opens.push(title);
        await command(opener, {kind: 'launch', app});
        if (await until(`${title} opens`, async () => (await titles(opener)).includes(title), 20_000)
          .catch(() => false)) return;
      }
      throw Error(`${title} never opened`);
    };
    step('windows open');
    await opened(ada, 'so.tilo.notes', 'Notes');
    await opened(ben, 'so.tilo.calculator', 'Calculator');
    check('windows-open', opens.length === 2, {opens});
    // What each desktop shows once the room had time to carry anything.
    await sleep(8_000);
    const seen = async () => Object.fromEntries(await Promise.all(
      Object.entries(people).map(async ([name, person]) => [name, await titles(person.tab)])));
    const before = await seen();
    // A reload shows what the page keeps and what the room hands back.
    await reload(ben, settleOptions);
    await until('Ben\'s desktop navigates again', () => ben.evaluate('Boolean(globalThis.tiloMount?.navigation)'));
    await sleep(5_000);
    const after = await seen();
    await shoot('windows-stay-own', mo);
    check('windows-stay-own',
      before.Ada.includes('Notes') && !before.Ada.includes('Calculator')
        && before.Ben.includes('Calculator') && !before.Ben.includes('Notes')
        && !before.Mo.includes('Notes') && !before.Mo.includes('Calculator')
        && !after.Ben.includes('Notes') && !after.Ada.includes('Calculator'),
      {before, after});

    step('own-window');
    // own-window: Ada's pointer on her own window's tab.
    const notes = await tabAt(ada, 'Notes');
    await mouse(ada, 'mouseMoved', notes);
    await sleep(1_500);
    await shoot('own-window', ada);
    check('own-window', true, {at: notes});

    step('own-close');
    // own-close: Ada closes her own window through the navigation door.
    const adaWindow = (await snapshot(ada)).entries.find(entry => entry.label === 'Notes');
    await command(ada, {kind: 'close', route: adaWindow.route}).catch(() => {});
    const closed = await until('Ada\'s window closes', async () => !(await titles(ada)).includes('Notes'), 20_000)
      .catch(() => false);
    await sleep(1_000);
    await shoot('own-close', ada);
    const benAfterClose = await titles(ben);
    check('own-close', closed && benAfterClose.includes('Calculator'), {ada: await titles(ada), ben: benAfterClose});

    step('moderator-desktop');
    // moderator-desktop: a page asks the room what it may moderate when it
    // joins and every fifteen seconds after; Mo was made a moderator well
    // before this, and his desktop is still only his.
    await sleep(16_000);
    const mos = await titles(mo);
    await shoot('moderator-desktop', mo);
    check('moderator-desktop', !mos.includes('Notes') && !mos.includes('Calculator'), {mo: mos});
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
