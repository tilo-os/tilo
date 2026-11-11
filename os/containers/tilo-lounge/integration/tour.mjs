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
import {chmodSync, mkdirSync, writeFileSync} from 'node:fs';
import {connect} from 'node:tls';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import * as waits from './waits.mjs';

const [site, relay, out, tiloBin, siteCa] = process.argv.slice(2);
if (!site || !relay || !out) {
  console.error('usage: node tour.mjs SITE_URL RELAY_ORIGIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
const report = {site, relay, screens: [], problems: []};
let shotNo = 0;

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
      const node = (n, depth) => { const b = n.bounds ?? n.rect; out.push([depth, n.role ?? '', n.name ?? '', n.value ?? '',
        b ? (Array.isArray(b) ? b : [b.x0 ?? b.x, b.y0 ?? b.y, b.x1 ?? b.x + b.width, b.y1 ?? b.y + b.height]).map(Math.round) : null]);
        (n.children || []).forEach(c => node(c, depth + 1)); };
      const walk = n => { if (n.semantics) node(n.semantics, 0); (n.children || []).forEach(walk); };
      const root = globalThis.tiloMount?.state()?.semantics?.root;
      if (root) walk(root);
      return JSON.stringify(out);
    })()`).catch(() => '[]'));
  };
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
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    for (const type of ['mousePressed', 'mouseReleased']) {
      await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
    }
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
  const quiet = async (ms = 1500) => {
    await waits.settle({evaluate: tab.evaluate.bind(tab), by: 3, owed: () => true, sleep, timeoutMs: 20_000});
    await sleep(ms);
  };
  const press = async control => {
    const tree = await names();
    const hit = tree.find(([, , n, , b]) => n === control && b);
    if (!hit) return false;
    const box = await canvasBox();
    const [x0, y0, x1, y1] = hit[4];
    await click(box.x + (x0 + x1) / 2, box.y + (y0 + y1) / 2);
    return true;
  };
  const step = async (what, run) => {
    try { await run(); } catch (error) {
      report.problems.push(`${name}: ${what}: ${error.message?.slice(0, 400)}`);
      console.log(`problem in ${name}: ${what}: ${error.message?.slice(0, 400)}`);
      await shoot(`problem-${what}`).catch(() => {});
    }
  };
  const mac = false;
  const search = async (query, pick = null) => {
    // Every search starts fresh: one left open by a row that keeps it up
    // (an answer) is closed first, so the query is not appended to it.
    if (await waits.searchOpen(tab)) {
      await key('Escape');
      await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: 10_000}).catch(() => {});
    }
    {
      const box = await canvasBox();
      await click(box.x + box.w / 2, box.y + box.h * 0.85);
      await key('k', mac ? 4 : 2);
    }
    await tab.waitFor('!!globalThis.tiloMount?.state()?.searchOpen', {timeout: 60_000});
    await type(query);
    await waits.settleSearch(tab, {sleep, timeoutMs: 30_000});
    await sleep(800);
    if (pick === null) return;
    await opened();
  };
  // Enter on the row the search offers, and wait for what it opens: the
  // search stays up until an app's window has arrived, which on a page
  // includes fetching the app's code.
  const opened = async () => {
    await key('Enter');
    await tab.waitFor('!globalThis.tiloMount?.state()?.searchOpen', {timeout: 90_000})
      .catch(() => report.problems.push(`${name}: the search stayed open after Enter`));
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
    const match = prompt.match(/tilo login (\S+)(?: --site (\S+?))?(?:,|\s|$)/);
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
    await step('chat', async () => { await search('chat', 1); await quiet(); await shoot('chat'); });
    await tab.close().catch(() => {});
    return;
  }
  await step('search-empty', async () => { await search(''); await shoot('search-open'); await key('Escape'); await quiet(500); });
  for (const app of ['Notes', 'Sheets', 'Files', 'Chat', 'Canvas', 'Word Processor', 'Calendar', 'Browser']) {
    await step(`open-${app}`, async () => {
      await search(app.toLowerCase());
      if (app === 'Notes') await shoot(`search-${app}`);
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
    if (!await press('Workspace: workspaces, overview and strips')) throw new Error('no workspace menu');
    await quiet(1500); await shoot('workspace-menu');
    await key('Escape'); await quiet(500);
  });
  await step('own-card', async () => {
    const tree = await names();
    const me = tree.find(([, , n]) => / \(you\): /.test(n));
    if (!me || !await press(me[2])) throw new Error('no own card');
    await quiet(1500); await shoot('own-card');
    await key('Escape'); await quiet(500);
  });
  await step('invite', async () => { await search('invite'); await shoot('search-invite'); await opened(); await quiet(); await shoot('invite'); });
  agent?.stop();
  await tab.close().catch(() => {});
};

await tour({name: 'desk', width: 1280, height: 800});
await tour({name: 'dark', width: 1280, height: 800, scheme: 'dark'});
await tour({name: 'phone', width: 390, height: 844, mobile: true});
writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
await browser.close().catch(() => {});
console.log(JSON.stringify({screens: report.screens.length, problems: report.problems}));
