// A person sets Tilo up through their agent, as the first screen asks: a
// fresh browser opens the site, the person clicks "Set up with your agent"
// and the page copies a prompt. What the prompt tells an agent to run runs
// here, headless, as an agent would run it: `tilo login <code>` with the
// `tilo` command this commit builds (the installer's download is the release
// pipeline's to prove, not this gate's). The tab must follow each step and
// end in the person's own workspace, with the agent there and what it does
// shown beside its name.
//
// Then the same person comes back on a browser that keeps nothing of them
// (a fresh profile, as after clearing a tab's storage): "Sign in with your
// agent" copies a second prompt, `tilo login <code>` on the computer that is
// already theirs approves it, and the new tab is signed in as that person.
//
// The agent is Claude Code as the `tilo` command finds it: `CLAUDECODE=1` in
// its environment and a `claude` on PATH that records what it was asked, so
// the run proves the command registered its tools without touching anyone's
// real agent. The command trusts the site's and the relay's test certificates
// only through `TILO_TEST_CA_FILE`, which production never sets.
//
// usage: node agent-setup.mjs SITE_URL RELAY_ORIGIN TILO_BIN SITE_CA OUT_DIR
//
// Prints a JSON report and saves what each page showed; exits non-zero when a
// step of either path did not happen.
import {execFile, spawn} from 'node:child_process';
import {chmodSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {connect} from 'node:tls';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, tiloBin, siteCa, out] = process.argv.slice(2);
if (!site || !relay || !tiloBin || !siteCa || !out) {
  console.error('usage: node agent-setup.mjs SITE_URL RELAY_ORIGIN TILO_BIN SITE_CA OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const report = {site, relay, at: new Date().toISOString(), steps: []};
const tabs = [];
const browsers = [];
// As the gate's other pages: the test certificates, a software GPU, a job container's small
// /dev/shm, and no sandbox where the gate runs as root.
const browserArgs = [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
];

const finish = async (code, message) => {
  if (message) report.error = message;
  for (const {name, tab} of tabs) {
    writeFileSync(join(out, `${name}.log`), (tab.log ?? []).join('\n'));
    const shot = await tab.screenshot().catch(() => null);
    if (shot) writeFileSync(join(out, `${name}.png`), shot);
    report[`${name}Text`] = await pageText(tab);
    // What the page kept of the workspaces it entered and the hosts it is a
    // member of: which way it took in on its next load.
    report[`${name}People`] = await tab.evaluate(`JSON.stringify(globalThis.tiloMount?.people?.() ?? null)`).catch(() => '');
    report[`${name}Kept`] = await tab.evaluate(`JSON.stringify(Object.fromEntries(Object.keys(localStorage)
      .filter(key => /workspace|membership|entered/i.test(key)).map(key => [key, localStorage.getItem(key)])))`).catch(() => '');
  }
  writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
  for (const browser of browsers) await browser.close().catch(() => {});
  if (message) {
    for (const {name, tab} of tabs) {
      const lines = (tab.log ?? []).slice(-60);
      if (lines.length) console.error(`--- ${name} said:\n${lines.join('\n')}`);
    }
    for (const {name} of tabs) console.error(`--- ${name} kept: ${report[`${name}Kept`]}\n--- ${name} people: ${report[`${name}People`]}`);
    console.error(`FAIL: ${message}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const fail = message => finish(1, message);

const pageText = tab => tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim()`).catch(() => '');
// Wait until `expression` holds in the page, or fail naming what never came.
const until = async (tab, expression, what, timeout = 120_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) await fail(`${what} never happened: ${(await pageText(tab)).slice(0, 600)}`);
    await sleep(250);
  }
};

// The relay's own certificate, as its listener presents it: the run hands it
// to the command as one it trusts exactly, beside the site's test CA.
const relayCertificate = () => new Promise((resolve, reject) => {
  const {hostname, port} = new URL(relay);
  const socket = connect({host: hostname, port: Number(port || 443), servername: hostname === '127.0.0.1' ? undefined : hostname,
    rejectUnauthorized: false}, () => {
    const der = socket.getPeerCertificate(false).raw;
    socket.end();
    const body = der.toString('base64').match(/.{1,64}/g).join('\n');
    resolve(`-----BEGIN CERTIFICATE-----\n${body}\n-----END CERTIFICATE-----\n`);
  });
  socket.on('error', reject);
});

// The computer the agent runs on: its own home, a `claude` that records how
// it was asked to add Tilo's tools, and Claude Code's environment marker.
const computer = join(out, 'computer');
mkdirSync(join(computer, 'bin'), {recursive: true});
const claudeCalls = join(computer, 'claude-calls.txt');
writeFileSync(join(computer, 'bin', 'claude'), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${claudeCalls}'\nexit 0\n`);
chmodSync(join(computer, 'bin', 'claude'), 0o755);
const relayPem = join(computer, 'relay.pem');
writeFileSync(relayPem, await relayCertificate().catch(error => fail(`the relay's certificate: ${error.message}`)));
const env = {
  PATH: `${join(computer, 'bin')}:${process.env.PATH}`,
  HOME: computer,
  XDG_STATE_HOME: join(computer, 'state'),
  XDG_CONFIG_HOME: join(computer, 'config'),
  CLAUDECODE: '1',
  TILO_TEST_CA_FILE: `${siteCa}:${relayPem}`,
};
// Run one `tilo …` as the agent does: its words, as the agent reads them.
const tilo = (args, {input} = {}) => new Promise(resolve => {
  const child = execFile(tiloBin, args, {env, timeout: 180_000, maxBuffer: 1 << 20}, (error, stdout, stderr) =>
    resolve({code: error ? (error.code ?? 1) : 0, stdout, stderr}));
  if (input !== undefined) { child.stdin.end(input); }
});

// One browser per person-visit, its first screen not yet seen, allowed to
// read back what the page copies.
const visit = async name => {
  const browser = await launch({args: browserArgs});
  browsers.push(browser);
  const tab = await browser.tab({firstRun: true});
  tabs.push({name, tab});
  // The tab's whole console and every request that failed, beside the
  // errors the harness keeps: a step that stops leaves what led to it.
  tab.log ??= [];
  const asked = new Map();
  browser.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
    else if (method === 'Network.loadingFailed') tab.log.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}`);
    else if (method === 'Runtime.consoleAPICalled' && !['error', 'warning'].includes(params.type)) {
      tab.log.push(`${params.type.toUpperCase()} ${params.args.map(a => a.value ?? a.description ?? '').join(' ')}`.slice(0, 600));
    }
  });
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false}).catch(() => {});
  await tab.send('Browser.grantPermissions', {origin: new URL(site).origin,
    permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  await tab.navigate(site);
  await until(tab, `document.documentElement.dataset.firstRun !== undefined && !!document.getElementById('first-run-agent')`,
    `${name}: the first screen`);
  return tab;
};
// A person's click on the button `id`: a real pointer press at its centre,
// so the page copies inside a user gesture, as it must. The button must be
// what is on top there the moment it shows: a layer over it (one still fading
// out, say) would take a person's click, and fails the run by its name.
const click = async (tab, id) => {
  const box = JSON.parse(await until(tab, `(() => {
    const element = document.getElementById(${JSON.stringify(id)});
    if (!element || element.disabled) return '';
    const rect = element.getBoundingClientRect();
    if (!rect.width || !rect.height) return '';
    const x = rect.x + rect.width / 2, y = rect.y + rect.height / 2;
    const top = document.elementFromPoint(x, y);
    const name = node => node ? node.tagName.toLowerCase() + (node.id ? '#' + node.id : '') + [...node.classList].map(c => '.' + c).join('') : 'nothing';
    return JSON.stringify({x, y, covered: top && element.contains(top) ? null : name(top)});
  })()`, `the button #${id}`));
  if (box.covered) await fail(`#${id} is covered by ${box.covered}: a person's click would land there`);
  for (const type of ['mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x: box.x, y: box.y, button: 'left', clickCount: 1});
  }
};
// What the page put on the clipboard, once it did.
const copied = (tab, what) => until(tab, `navigator.clipboard.readText().then(text => text.includes('tilo login') ? text : '', () => '')`,
  `${what} on the clipboard`);
// The code and site a prompt names.
const commandOf = prompt => {
  const match = prompt.match(/tilo login ([2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4})(?: --site (\S+?))?(?:,|\s|$)/);
  if (!match) return null;
  return {code: match[1], site: match[2] ?? null};
};
// The steps the tab shows, as it shows them.
const followSteps = async (tab, until) => {
  const seen = [];
  const deadline = Date.now() + 180_000;
  for (;;) {
    const text = await tab.evaluate(`document.getElementById('first-run-steps')?.innerText ?? ''`).catch(() => '');
    for (const line of text.split('\n').map(line => line.trim()).filter(Boolean)) if (!seen.includes(line)) seen.push(line);
    if (await tab.evaluate(until).catch(() => false)) return seen;
    if (Date.now() > deadline) return seen;
    await sleep(200);
  }
};
// The person the page is, by their root, and the workspace in front.
// The person is who the page keeps its memberships as, on the relay's host.
const person = tab => tab.evaluate(`JSON.stringify({
  user: (() => { try {
    const all = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}');
    return all[${JSON.stringify(new URL(relay).origin)}]?.userPublicKey ?? null;
  } catch { return null; } })(),
  title: document.title,
  room: (() => { try { return JSON.parse(globalThis.localStorage.getItem('tilo-chosen-workspace') ?? 'null')?.room ?? null; } catch { return null; } })(),
  people: (() => { try { return globalThis.tiloMount?.people?.() ?? null; } catch { return null; } })(),
})`).then(JSON.parse).catch(() => ({}));

// 1. Set up through the agent.
const first = await visit('setup');
await click(first, 'first-run-agent');
await until(first, `!!document.documentElement.dataset.firstRun`, 'the first screen answering the click on Set up');
const prompt = await copied(first, 'the setup prompt');
report.prompt = prompt;
const setup = commandOf(prompt);
if (!setup) await fail(`the prompt names no tilo login code: ${prompt}`);
if (!/^Set up Tilo for me: run curl -fsSL \S+ \| sh && tilo login /.test(prompt) || prompt.length > 200) {
  await fail(`the prompt is not the short one-line setup: ${prompt}`);
}
if (setup.site && new URL(setup.site).origin !== new URL(site).origin) await fail(`the prompt names another site: ${setup.site}`);
const login = tilo(['login', setup.code, '--site', setup.site ?? site]);
const steps = await followSteps(first, `document.documentElement.dataset.firstRun === undefined || document.documentElement.dataset.firstRun === 'connected'`);
const loggedIn = await login;
report.login = loggedIn;
report.steps = steps;
if (loggedIn.code !== 0) await fail(`tilo login failed (${loggedIn.code}): ${loggedIn.stderr}${loggedIn.stdout}`);
if (!/restart/.test(loggedIn.stdout) || !/\btilo_[a-z]+\b/.test(loggedIn.stdout)) {
  await fail(`tilo login did not tell the agent to restart and which tool to call: ${loggedIn.stdout}`);
}
if (/failed/i.test(loggedIn.stdout)) await fail(`tilo login said something failed: ${loggedIn.stdout}`);
const registered = (() => { try { return readFileSync(claudeCalls, 'utf8'); } catch { return ''; } })();
report.claude = registered;
if (!/mcp add .*tilo .*-- \S*tilo mcp/.test(registered)) await fail(`tilo login did not add its MCP server to Claude Code: ${registered || 'never asked'}`);
if (steps.length < 2) await fail(`the tab showed no steps as the agent went: ${JSON.stringify(steps)}`);

// The screen leaves for the person's own workspace.
await until(first, `document.documentElement.dataset.firstRun === undefined`, 'the first screen leaving', 60_000);
// Titled after the person's own workspace once it says its name, never the Lounge's.
const ownTitle = `/^Tilo · /.test(document.title) && !/lounge/i.test(document.title)`;
for (const deadline = Date.now() + 30_000; !await first.evaluate(ownTitle).catch(() => false);) {
  if (Date.now() > deadline) await fail(`the tab is titled "${await first.evaluate('document.title')}", not after the person's own workspace`);
  await sleep(250);
}
report.workspace = await first.evaluate('document.title');

// The agent is present once its MCP server connects, as Claude Code starts it
// once restarted, and what it does shows beside its name: one tool call, said
// as the workspace says it ("Looking at the apps" for a look at its apps).
// Who is here is live presence alone: the agent's own, on the workspace's
// live plane.
const doing = 'Looking at the apps';
// The agent's MCP server stays up as a restarted agent keeps it, while the
// tab is looked at; its answers are kept for the report.
const server = spawn(tiloBin, ['mcp'], {env, stdio: ['pipe', 'pipe', 'pipe']});
let answers = '';
server.stdout.on('data', chunk => { answers += chunk; report.mcp = answers.slice(-2000); });
server.stderr.on('data', chunk => { answers += chunk; report.mcp = answers.slice(-2000); });
for (const line of [
  {jsonrpc: '2.0', id: 1, method: 'initialize', params: {protocolVersion: '2025-06-18', capabilities: {}, clientInfo: {name: 'gate', version: '1'}}},
  {jsonrpc: '2.0', method: 'notifications/initialized'},
  {jsonrpc: '2.0', id: 2, method: 'tools/call', params: {name: 'tilo_ls', arguments: {}}},
]) server.stdin.write(`${JSON.stringify(line)}\n`);
const beside = `(() => { const row = (globalThis.tiloMount?.people?.() ?? []).find(person => person.agent && /Claude Code/.test(person.label ?? '')); return !!row && JSON.stringify(row).includes(${JSON.stringify(doing)}); })()`;
await until(first, beside, `Claude Code present, with "${doing}" beside its name`, 90_000);
report.mcp = answers.slice(0, 2000);
const signedUp = await person(first);
report.person = signedUp;

// Loaded again while the agent's MCP server is still connected, the tab is the
// same person in the same workspace, titled after it, with the agent still
// there.
await first.reload();
await until(first, `!!globalThis.tiloMount && JSON.stringify(globalThis.tiloMount.people?.() ?? null).includes('Claude Code')`,
  'Claude Code present in the workspace after a reload', 60_000);
server.stdin.end();
const reloaded = await person(first);
report.reloaded = reloaded;
if (!signedUp.user || reloaded.user !== signedUp.user) {
  await fail(`after a reload the tab is ${reloaded.user ?? 'nobody'}, not the person who set up (${signedUp.user ?? 'unknown'})`);
}
if (!signedUp.room || reloaded.room !== signedUp.room) {
  await fail(`after a reload the tab is in ${reloaded.room ?? 'no workspace'}, not the person's own (${signedUp.room ?? 'unknown'})`);
}
if (/lounge/i.test(reloaded.title) || reloaded.title !== signedUp.title) {
  await fail(`after a reload the tab is titled "${reloaded.title}", not "${signedUp.title}"`);
}

// 2. Back on a browser that keeps nothing: signed in through the agent.
const second = await visit('returning');
await click(second, 'first-run-agent-sign-in');
await until(second, `!!document.documentElement.dataset.firstRun`, 'the first screen answering the click on Sign in with your agent');
const again = await copied(second, 'the sign-in prompt');
report.signInPrompt = again;
const signIn = commandOf(again);
if (!signIn) await fail(`the sign-in prompt names no tilo login code: ${again}`);
const approved = await tilo(['login', signIn.code, '--site', signIn.site ?? site]);
report.approve = approved;
if (approved.code !== 0) await fail(`tilo login did not approve the tab (${approved.code}): ${approved.stderr}${approved.stdout}`);
await until(second, `document.documentElement.dataset.firstRun === undefined && !!globalThis.tiloMount`,
  'the returning tab signed in', 180_000);
const back = await person(second);
report.returned = back;
if (!signedUp.user || back.user !== signedUp.user) {
  await fail(`the returning tab is ${back.user ?? 'nobody'}, not the person who set up (${signedUp.user ?? 'unknown'})`);
}
// Signed in, it opens the person's own workspace, as the first tab did.
await until(second, `!/lounge/i.test(JSON.stringify({title: document.title, own: globalThis.tiloMount?.state?.()?.entry ?? null}))`,
  'the returning tab in the person\'s own workspace, not the Lounge', 60_000);
await finish(0);
