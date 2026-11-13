// A newcomer's tour of the web page, for a person to look at: the built
// bundle in headless Chromium against the Lounge's relay, walked through the
// screens a first-time visitor meets — the arrival, the desk, the search,
// each everyday app, a note written and renamed, Settings and History — then
// again in the dark appearance and at a phone's width. Every screen is kept as
// a picture with the names the desk published and what the console said, so
// a review reads what a person saw. It fails nothing on looks; it fails only
// when the page itself reports an exception or a panic.
//
// With TILO_BIN (and SITE_CA) the newcomer takes the door's agent path: the
// prompt it copies runs here as an agent runs it (`tilo login`), with a stand-in
// `claude` that only records how it was asked, and the agent then works in the
// workspace through `tilo mcp` while the tab is looked at.
//
// usage: node tour.mjs SITE_URL RELAY_ORIGIN OUT_DIR [TILO_BIN SITE_CA]
import {execFile, spawn} from 'node:child_process';
import {chmodSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {connect} from 'node:tls';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import * as waits from './waits.mjs';
import {clock} from './timing.mjs';

const [site, relay, out, tiloBin, siteCa] = process.argv.slice(2);
if (!site || !relay || !out) {
  console.error('usage: node tour.mjs SITE_URL RELAY_ORIGIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const mark = clock();
const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
mark('the browser started');
const report = {site, relay, screens: [], problems: []};
// The screens are numbered as they are taken, across the three appearances, so
// a listing of them reads in the order a newcomer met the site.
let shotNo = 0;
// What the tour spent waiting for the desk to stand still: a fixed pause after
// each step adds up over three appearances, and this says how much of the tour
// it was, and what each wait ended on.
let quietMs = 0;
const quietHow = {};

// The apps a person's everyday round opens here, in the order the tour walks them.
const everyday = ['Notes', 'Sheets', 'Files', 'Chat', 'Canvas', 'Word Processor', 'Calendar', 'Browser'];
// What this page has, by name, as its bundle lists it (`apps.json`): the apps a window here can
// show. An app that runs only in Tilo on a computer has no row in the page's search and no window
// to open, so the tour never asks for one — searching for it would walk a person into a card that
// says nothing matched, and pressing Enter there rightly does nothing.
const bundle = process.env.TILO_BUNDLE;
const pageApps = bundle
  ? (() => { try { return JSON.parse(readFileSync(join(bundle, 'apps.json'), 'utf8')); } catch { return null; } })()
  : null;
if (bundle && !pageApps?.applications?.length) {
  report.problems.push(`the bundle at ${bundle} lists no apps, so the tour cannot tell which this page can open`);
}
const pageHas = name => !pageApps?.applications || pageApps.applications.some(app => app.name === name);

// The computer the agent runs on, as agent-setup.mjs sets it up: its own home,
// a `claude` that records how it was asked to add Tilo's tools, and the relay's
// test certificate beside the site's.
const relayPem = () => new Promise((resolve, reject) => {
  const {hostname, port} = new URL(relay);
  const socket = connect({host: hostname, port: Number(port || 443), servername: hostname, rejectUnauthorized: false}, () => {
    const der = socket.getPeerCertificate(true).raw;
    socket.end();
    resolve(`-----BEGIN CERTIFICATE-----\n${der.toString('base64').match(/.{1,64}/g).join('\n')}\n-----END CERTIFICATE-----\n`);
  });
  socket.on('error', reject);
});
let agentEnv = null;
if (tiloBin) {
  const computer = join(out, '..', 'computer');
  mkdirSync(join(computer, 'bin'), {recursive: true});
  writeFileSync(join(computer, 'bin', 'claude'), `#!/bin/sh
[ "$1 $2" = "mcp add" ] || exit 0
printf '{"mcpServers":{"tilo":{"type":"stdio","command":"${tiloBin}","args":["mcp"],"env":{}}}}\\n' > "$HOME/.claude.json"
`);
  chmodSync(join(computer, 'bin', 'claude'), 0o755);
  writeFileSync(join(computer, 'relay.pem'), await relayPem());
  agentEnv = {
    PATH: `${join(computer, 'bin')}:${process.env.PATH}`,
    HOME: computer,
    XDG_STATE_HOME: join(computer, 'state'),
    XDG_CONFIG_HOME: join(computer, 'config'),
    CLAUDECODE: '1',
    TILO_TEST_CA_FILE: `${siteCa}:${join(computer, 'relay.pem')}`,
  };
}
const tilo = args => new Promise(resolve => {
  execFile(tiloBin, args, {env: agentEnv, timeout: 180_000, maxBuffer: 1 << 20}, (error, stdout, stderr) =>
    resolve({code: error ? (error.code ?? 1) : 0, stdout, stderr}));
});
// The agent's MCP server, as a restarted agent keeps it: one call at a time,
// each answer's text returned.
const mcp = () => {
  const server = spawn(tiloBin, ['mcp'], {env: agentEnv, stdio: ['pipe', 'pipe', 'pipe']});
  let buffered = '';
  const waiting = new Map();
  server.stdout.on('data', chunk => {
    buffered += chunk;
    for (let at; (at = buffered.indexOf('\n')) >= 0;) {
      const line = buffered.slice(0, at);
      buffered = buffered.slice(at + 1);
      try { const message = JSON.parse(line); waiting.get(message.id)?.(message); } catch {}
    }
  });
  server.stderr.on('data', chunk => console.log(`  mcp: ${String(chunk).trim().slice(0, 300)}`));
  let id = 0;
  const ask = (method, params) => new Promise(resolve => {
    const n = ++id;
    const timer = setTimeout(() => resolve({error: 'no answer in 120s'}), 120_000);
    waiting.set(n, message => { clearTimeout(timer); resolve(message); });
    server.stdin.write(`${JSON.stringify({jsonrpc: '2.0', id: n, method, params})}\n`);
  });
  return {
    start: async () => {
      await ask('initialize', {protocolVersion: '2025-06-18', capabilities: {}, clientInfo: {name: 'tour', version: '1'}});
      server.stdin.write(`${JSON.stringify({jsonrpc: '2.0', method: 'notifications/initialized'})}\n`);
    },
    call: async (name, args) => {
      const answer = await ask('tools/call', {name, arguments: args});
      const text = (answer.result?.content ?? []).map(part => part.text ?? '').join('\n') || JSON.stringify(answer.error ?? answer);
      console.log(`  agent ${name} ${JSON.stringify(args)} -> ${answer.result?.isError ? 'ERROR ' : ''}${text.replace(/\s+/g, ' ').slice(0, 600)}`);
      return text;
    },
    stop: () => server.kill(),
  };
};

const tour = async ({name, width, height, mobile = false, scheme = 'light'}) => {
  const tab = await browser.tab({firstRun: true});
  await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 1, mobile});
  if (mobile) await tab.send('Emulation.setTouchEmulationEnabled', {enabled: true}).catch(() => {});
  await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
  let logSeen = 0;
  const names = async () => {
    await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
    await sleep(300);
    return JSON.parse(await tab.evaluate(`(() => {
      const out = [];
      const node = (n, depth) => {
        if (!n) return; const b = n.bounds ?? n.rect; out.push([depth, n.role ?? '', n.name ?? '', n.value ?? '',
        b ? (Array.isArray(b) ? b : [b.x0 ?? b.x, b.y0 ?? b.y, b.x1 ?? b.x + b.width, b.y1 ?? b.y + b.height]).map(Math.round) : null]);
        (n.children || []).forEach(c => node(c, depth + 1)); };
      const walk = n => { if (n.semantics) node(n.semantics, 0); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state()?.semantics?.root;
      if (root) walk(root);
      return JSON.stringify(out);
    })()`).catch(() => '[]'));
  };
  // The open search's card, as the page publishes it: the query its field holds and the rows it
  // offers, each as a name with what it says under it — a row that could not be carried out says
  // why there.
  const card = async () => tab.evaluate(`(() => {
    const state = globalThis.tiloMount && globalThis.tiloMount.state ? globalThis.tiloMount.state() : null;
    return JSON.stringify({
      open: state ? state.searchOpen === true : null,
      searching: state ? state.searchSearching === true : null,
      card: state ? state.searchCard || null : null,
    });
  })()`).catch(error => `unreadable: ${String(error && error.message ? error.message : error).slice(-300)}`);
  const shoot = async label => {
    const file = `${String(++shotNo).padStart(2, '0')}-${name}-${label.replace(/[^a-z0-9]+/gi, '-')}`;
    // WebP keeps forty screens small enough to read back from a log.
    const shot = await tab.send('Page.captureScreenshot', {format: 'webp', quality: 60}).catch(() => null);
    if (shot?.result?.data) writeFileSync(join(out, `${file}.webp`), Buffer.from(shot.result.data, 'base64'));
    const said = tab.log.slice(logSeen);
    logSeen = tab.log.length;
    const text = await tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 1500)`).catch(() => '');
    const tree = await names();
    writeFileSync(join(out, `${file}.json`), JSON.stringify({label, said, text, tree}, null, 1));
    report.screens.push({file, label, said: said.slice(0, 20)});
    console.log(`shot ${file}`);
    for (const line of said.slice(0, 6)) console.log(`  said: ${line.slice(0, 400)}`);
  };
  const canvasBox = async () => JSON.parse(await tab.evaluate(`JSON.stringify((() => {
    const r = globalThis.tiloMount?.canvas?.getBoundingClientRect();
    return r ? {x: r.x, y: r.y, w: r.width, h: r.height} : {x: 0, y: 0, w: ${width}, h: ${height}};
  })())`));
  const click = async (x, y) => {
    if (mobile) {
      // A phone is touched, not clicked.
      await tab.send('Input.dispatchTouchEvent', {type: 'touchStart', touchPoints: [{x, y}]});
      await tab.send('Input.dispatchTouchEvent', {type: 'touchEnd', touchPoints: []});
      await sleep(700);
      return;
    }
    // Moved twice, so the desk sees the pointer arrive even where it already was; pressed with
    // the button held, as a person's press is (a press that says no button is held is a hover).
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: x - 1, y});
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    await sleep(300);
    await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', buttons: 1, clickCount: 1});
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', buttons: 0, clickCount: 1});
    await sleep(700);
  };
  const key = async (k, modifiers = 0) => {
    const named = {Enter: 13, Escape: 27, ArrowDown: 40, ArrowUp: 38, Backspace: 8, Tab: 9, F2: 113};
    const code = k.length === 1 ? `Key${k.toUpperCase()}` : k;
    const event = {key: k, code, windowsVirtualKeyCode: named[k] ?? k.toUpperCase().charCodeAt(0), modifiers};
    await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', ...event, ...(k === 'Enter' ? {text: '\r'} : {})});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...event});
    await sleep(300);
  };
  const type = async text => {
    for (const ch of text) {
      await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
      await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
    }
    await sleep(600);
  };
  // The desk standing still before a picture: three frames, or the silence of
  // a desk with nothing left to draw (`idleMs`), then a moment as a person
  // would look at it. Waiting only for frames costs the whole timeout on every
  // screen a still desk drew nothing for.
  const quiet = async (ms = 400) => {
    const began = Date.now();
    const how = await waits.settle({evaluate: tab.evaluate.bind(tab), by: 3, owed: () => true, idleMs: 2500, sleep, timeoutMs: 20_000});
    quietHow[how] = (quietHow[how] ?? 0) + 1;
    await sleep(ms);
    quietMs += Date.now() - began;
  };
  // Press the control named `control` (a name, or a test of one) where the desk last laid it
  // out; `where` narrows it to the place it stands, such as the top bar.
  const press = async (control, where = () => true) => {
    const tree = await names();
    const named = typeof control === 'function' ? control : n => n === control;
    const box = await canvasBox();
    // Only a control on screen can be pressed: one laid out where the strip has scrolled it
    // away is no place a person can reach, and a press beside the page lands nowhere.
    const onScreen = ([x0, y0, x1, y1]) => x1 > x0 && y1 > y0 && (x0 + x1) / 2 >= 0 && (x0 + x1) / 2 < box.w
      && (y0 + y1) / 2 >= 0 && (y0 + y1) / 2 < box.h;
    const hit = tree.find(([, , n, , b]) => named(n) && b && where(b) && onScreen(b));
    if (!hit) return false;
    const [x0, y0, x1, y1] = hit[4];
    const [x, y] = [box.x + (x0 + x1) / 2, box.y + (y0 + y1) / 2];
    // What a press there reaches: the page's canvas, or something of the page over it.
    const under = await tab.evaluate(`(() => { const e = document.elementFromPoint(${x}, ${y});
      return e === globalThis.tiloMount?.canvas ? 'canvas' : e ? e.localName + (e.id ? '#' + e.id : '') + (e.className ? '.' + String(e.className).split(' ')[0] : '') : 'nothing'; })()`).catch(() => '?');
    console.log(`  press ${JSON.stringify(hit[2])} at ${Math.round(x)},${Math.round(y)} on ${under}`);
    await click(x, y);
    return true;
  };
  const step = async (what, run) => {
    const began = Date.now();
    try { await run(); } catch (error) {
      report.problems.push(`${name}: ${what}: ${error.message?.slice(0, 400)}`);
      console.log(`problem in ${name}: ${what}: ${error.message?.slice(0, 400)}`);
      await shoot(`problem-${what}`).catch(() => {});
    }
    console.log(`TIMING tour=${name} step=${what} seconds=${((Date.now() - began) / 1000).toFixed(1)}`);
  };
  const mac = false;
  // Search's own Escape clears the query and the next closes it, so a search left standing with
  // a query takes two: press until it is gone. Whether it went.
  const closeSearch = async () => {
    for (let pressed = 0; pressed < 3; pressed++) {
      if (!await waits.searchOpen(tab)) return true;
      await key('Escape');
      if (await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: 10_000}).then(() => true, () => false)) return true;
    }
    return !await waits.searchOpen(tab);
  };
  const search = async (query, pick = null) => {
    // Every search starts fresh: one left open by a row that keeps it up
    // (an answer) is closed first, so the query is not appended to it.
    if (!await closeSearch()) {
      report.problems.push(`${name}: Escape left the search open`);
      await shoot('search-stuck');
    }
    // The keyboard goes to the desk without a click: a click to give it focus lands on whatever
    // stands there, a row of a search still up or a window's button.
    await tab.evaluate('globalThis.tiloMount?.canvas?.focus({preventScroll: true})').catch(() => {});
    await key('k', mac ? 4 : 2);
    await tab.waitFor('!!globalThis.tiloMount?.state()?.searchOpen', {timeout: 60_000});
    await type(query);
    await waits.settleSearch(tab, {sleep, timeoutMs: 30_000});
    // A source answers on a worker, so the card may still be filling in when the frames moved:
    // wait for it to say it owes nothing, which is when it has the rows a person presses.
    await tab.waitFor('globalThis.tiloMount?.state()?.searchSearching !== true', {timeout: 30_000})
      .catch(() => {});
    await sleep(800);
    if (pick === null) return;
    await opened();
  };
  // Enter on the row the search offers, and wait for what it opens: the
  // search stays up until an app's window has arrived, which on a page
  // includes fetching the app's code.
  const opened = async (press = true) => {
    // What Enter is about to choose, for the log: the card's own rows and the query it holds.
    const before = await card();
    console.log(`  card: ${before}`);
    // A card kept for the log alone says what it offered and presses nothing.
    if (!press) return;
    // A card that found nothing has no row for Enter to open, and pressing it would only leave the
    // search standing: the step says what it could not find instead.
    const shown = typeof before === 'string' ? JSON.parse(before) : null;
    if (shown?.card && shown.card.rows.length === 0) {
      report.problems.push(`${name}: nothing matched “${shown.card.query}”`);
      return;
    }
    await key('Enter');
    const closed = await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: 90_000})
      .then(() => true, () => false);
    if (!closed) {
      // What it stood on: a card that found nothing has no row Enter could open — it says so — and
      // that is a query this page has no answer to, not a search that would not go.
      const after = await card();
      const shown = typeof after === 'string' ? JSON.parse(after) : null;
      if (shown?.card && shown.card.rows.length === 0) {
        report.problems.push(`${name}: nothing matched “${shown.card.query}”`);
      } else {
        report.problems.push(`${name}: the search stayed open after Enter`);
      }
      console.log(`  search stayed open: ${after}`);
    }
    await sleep(500);
    // What the desk holds now, and what the page last said, for the log.
    const held = await tab.evaluate(`JSON.stringify({
      windows: (globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).map(e => (e.app ?? '?') + ':' + (e.label ?? '')),
      status: document.getElementById('status')?.textContent?.trim().slice(0, 200) ?? null,
    })`).catch(() => 'unreadable');
    console.log(`  held: ${held}`);
  };

  // The arrival, as a first-time visitor's tab meets it.
  await tab.navigate(site);
  await sleep(1500);
  await shoot('arriving');
  await tab.waitFor(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, {timeout: 180_000})
    .catch(() => report.problems.push(`${name}: the page never arrived`));
  await quiet(3000);
  await shoot('arrived');
  // A newcomer at the door sets Tilo up through their agent when the tour
  // has the command, and else skips: either way the page opens their own
  // workspace.
  let agent = null;
  if (agentEnv && name === 'desk' && await tab.evaluate(`!!document.getElementById('first-run-agent')?.offsetParent`).catch(() => false)) {
    await tab.waitFor(`document.getElementById('first-run')?.dataset.setup === 'ready'`, {timeout: 60_000}).catch(() => {});
    const prompt = await tab.evaluate(`document.getElementById('first-run-offer')?.textContent ?? ''`);
    await tab.evaluate(`document.getElementById('first-run-agent').click()`);
    await sleep(1500);
    await shoot('agent-waiting');
    const match = prompt.match(/(?:tilo|sh -s) login (\S+)(?: --site (\S+?))?(?:,|\s|$)/);
    if (!match) report.problems.push(`desk: the prompt names no login code: ${prompt}`);
    else {
      const login = await tilo(['login', match[1], '--site', match[2] ?? site]);
      console.log(`  tilo login -> ${login.code}: ${(login.stdout + login.stderr).replace(/\s+/g, ' ').slice(0, 800)}`);
      await sleep(1500);
      await shoot('agent-connected');
      await tab.waitFor(`document.documentElement.dataset.firstRun === undefined`, {timeout: 90_000})
        .catch(() => report.problems.push('desk: the door never left after tilo login'));
      await tab.waitFor('!!globalThis.tiloMount?.state()?.frames', {timeout: 180_000}).catch(() => {});
      await quiet(4000);
      await shoot('own-workspace');
      agent = mcp();
      await agent.start();
      await agent.call('tilo_ls', {});
      await quiet(2000);
      await shoot('agent-here');
      await agent.call('tilo_ls', {path: 'notes'});
      await agent.call('tilo_do', {path: 'sheets#new-sheet', args: {name: 'Budget'}});
      await quiet(3000);
      await shoot('agent-made-a-sheet');
      await agent.call('tilo_do', {path: 'word-processor#append-paragraph', args: {text: 'Hello from the agent'}});
      await quiet(3000);
      await shoot('agent-wrote');
    }
  } else if (await tab.evaluate(`!!document.getElementById('first-run-skip')?.offsetParent`).catch(() => false)) {
    await tab.evaluate(`document.getElementById('first-run-skip').click()`);
    await sleep(2000);
    await shoot('skipped');
    await tab.waitFor('!!globalThis.tiloMount?.state()?.frames', {timeout: 180_000})
      .catch(() => report.problems.push(`${name}: no desk after skipping`));
    await quiet(4000);
    await shoot('own-workspace');
  }
  if (name !== 'desk') {
    // The other appearances look at the desk, the search, one app and Settings.
    await step('search', async () => { await search('notes'); await shoot('search-notes'); await opened(); await quiet(); await shoot('notes'); });
    await step('settings', async () => { await search('settings', 1); await quiet(); await shoot('settings'); });
  // The person's devices and the agents acting for them, found by name.
  await step('devices-access', async () => { await search('devices and access'); await shoot('search-devices-access'); await opened(); await quiet(); await shoot('devices-access'); });
    await step('chat', async () => { await search('chat', 1); await quiet(); await shoot('chat'); });
    await tab.close().catch(() => {});
    return;
  }
  await step('search-empty', async () => { await search(''); await shoot('search-open'); await key('Escape'); await quiet(500); });
  for (const app of everyday.filter(app => pageHas(app))) {
    await step(`open-${app}`, async () => {
      await search(app.toLowerCase());
      await shoot(`search-${app}`);
      await opened();
      await quiet(2500);
      await shoot(app);
    });
  }
  await step('new-note', async () => {
    await search('notes', 1); await quiet();
    if (!await press('New note')) throw new Error('no New note control');
    await quiet(); await type('Groceries'); await key('Enter'); await type('Milk and eggs'); await quiet();
    await shoot('note-written');
    await key('z', 2); await quiet(); await shoot('note-undo');
  });
  await step('rename', async () => { await search('rename'); await shoot('search-rename'); await key('Escape'); await quiet(500); });
  await step('settings', async () => { await search('settings', 1); await quiet(); await shoot('settings'); });
  await step('history', async () => { await search('history'); await shoot('search-history'); await opened(); await quiet(); await shoot('history'); });
  await step('workspaces', async () => { await search('workspace'); await shoot('search-workspace'); await key('Escape'); await quiet(500); });
  await step('tree', async () => {
    const tree = await names();
    writeFileSync(join(out, 'desk-tree.json'), JSON.stringify(tree, null, 1));
    // The bars' controls by name, for the next tour to press.
    const bars = tree.filter(([, , n, , b]) => n && b && b[1] < 80).map(([, role, n, , b]) => `${role}:${n}@${b[0]},${b[1]}`);
    console.log(`bars: ${[...new Set(bars)].join(' | ').slice(0, 3000)}`);
  });
  await step('workspace-menu', async () => {
    const topBar = b => b[1] < 32;
    if (!await press(n => n.endsWith(': workspaces, overview and strips'), topBar)) throw new Error('no workspace menu');
    await quiet(1500); await shoot('workspace-menu');
    console.log(`  menu: ${JSON.stringify((await names()).filter(([, role]) => /Menu/.test(role)).map(([, role, n]) => `${role}:${n}`).slice(0, 12))}`);
    await key('Escape'); await quiet(500);
  });
  await step('own-card', async () => {
    if (!await press(n => n.endsWith(' (you): your profile'), b => b[1] < 32)) throw new Error('no own card');
    await quiet(1500); await shoot('own-card');
    await key('Escape'); await quiet(500);
  });
  // What "invite" alone offers is kept as the card's own record: an app's invitation to one of
  // its own things (a calendar event, a contact) takes a row only where it can run now, so the
  // workspace's own verb stands where Enter would carry it out.
  await step('invite', async () => {
    await search('invite'); await quiet();
    await opened(false);
    await shoot('search-invite');
    await key('Escape'); await quiet(500);
  });
  // And Enter on the workspace's verb opens the invite sheet.
  await step('invite-people', async () => { await search('invite people'); await shoot('search-invite-people'); await opened(); await quiet(); await shoot('invite'); });
  agent?.stop();
  await tab.close().catch(() => {});
};

// One appearance after another: three desks drawn at once share one software
// rasteriser and one machine, and the tour then reads pages that never arrived
// (measured: 19 minutes with three at once against 23 one after another, and
// screens of a search that was still opening). One appearance that stops — a
// stalled page, a program that never answers — is a problem written down, not
// the end of the tour: the others are walked and kept as pictures all the same,
// and each is timed from the mark it leaves.
const appearance = async (options, at) => {
  try { await tour(options); } catch (error) {
    report.problems.push(`${options.name}: the tour stopped: ${error.message?.slice(0, 400) ?? error}`);
    console.log(`problem in ${options.name}: the tour stopped: ${error.message?.slice(0, 400) ?? error}`);
  }
  mark(at);
};
await appearance({name: 'desk', width: 1280, height: 800}, 'the desk appearance');
await appearance({name: 'dark', width: 1280, height: 800, scheme: 'dark'}, 'the dark appearance');
await appearance({name: 'phone', width: 390, height: 844, mobile: true}, 'the phone appearance');
writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
await browser.close().catch(() => {});
console.log(`TIMING quiet=${(quietMs / 1000).toFixed(1)}s of the tour was waiting for the desk to stand still (${Object.entries(quietHow).map(([how, n]) => `${n} ${how}`).join(', ')})`);
console.log(JSON.stringify({screens: report.screens.length, problems: report.problems}));
