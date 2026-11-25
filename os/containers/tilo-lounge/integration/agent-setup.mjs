// A person sets Tilo up through their agent, as the first screen asks: a
// fresh browser opens the site, the person clicks "Set up with your agent"
// and the page copies a prompt. What the prompt tells an agent to run runs
// here, headless, as an agent would run it: `tilo login <code>` with the
// `tilo` command this commit builds (the installer's download is the release
// pipeline's to prove, not this check's). The tab must follow each step and
// end in the person's own workspace, with the agent there and what it does
// shown beside its name.
//
// Then the same person comes back on a browser that keeps nothing of them
// (a fresh profile, as after clearing a tab's storage): "Sign in with your
// agent" copies a second prompt, `tilo login <code>` on the computer that is
// already theirs approves it, and the new tab is signed in as that person.
//
// The agent is any agent: it names itself to `tilo login`, adds Tilo's MCP
// server to its own configuration as the login's words say, and confirms it
// with `tilo agent check --config`, without touching anyone's real agent. The
// stack is plain HTTP on this machine, so the command needs
// no test certificate to reach it.
//
// usage: node agent-setup.mjs SITE_URL SERVER_ORIGIN TILO_BIN OUT_DIR
//
// Prints a JSON report and saves what each page showed; exits non-zero when a
// step of either path did not happen.
import {execFile, spawn} from 'node:child_process';
import {chmodSync, mkdirSync, readFileSync, readdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {clock} from './timing.mjs';
import {stepLimit, shellMetrics, machine, browserMemory, reloaded, keepRecoveryWords} from './waits.mjs';

const [site, server, tiloBin, out] = process.argv.slice(2);
if (!site || !server || !tiloBin || !out) {
  console.error('usage: node agent-setup.mjs SITE_URL SERVER_ORIGIN TILO_BIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const mark = clock();
const report = {site, server, at: new Date().toISOString(), steps: []};
const tabs = [];
const browsers = [];
// The stack is plain http on this machine, so no certificate stands in the
// page's way; this flow asks for Chromium's own log on stderr, where why a
// renderer or the GPU process died is said.
const browserArgs = ['--enable-logging=stderr', '--v=0'];

const finish = async (code, message) => {
  if (message) report.error = message;
  // Said first: what follows reads every tab, and a tab that stopped answering
  // must not keep the run from saying why it failed.
  if (message) console.error(`FAIL: ${message}`);
  if (message) console.error(`the machine as it failed: ${machine().text}`);
  // What the computer's peer (`tilo-runtime`) said, which serves the agents' calls.
  if (message) {
    try {
      const dir = join(computer, 'state', 'tilo', 'device');
      report.peer = readdirSync(dir).filter(name => /^session-.*\.log$/.test(name))
        .map(name => readFileSync(join(dir, name), 'utf8').slice(-4000)).join('\n');
    } catch (error) { report.peer = `not read: ${error.message}`; }
  }
  // And what each tab said, before anything asks a tab: a run past its limit
  // is stopped, and a stopped run says nothing it had not said yet.
  if (message) {
    for (const {name, tab} of tabs) {
      const lines = (tab.log ?? []).slice(-60);
      if (lines.length) console.error(`--- ${name} said:\n${lines.join('\n')}`);
    }
  }
  // Each read of a tab is short: a page that answers does at once, and one
  // that stopped answering would hold every read to the protocol's own limit.
  const read = (tab, expression) => briefly(tab, expression, 'no answer');
  for (const {name, tab, browser} of tabs) {
    writeFileSync(join(out, `${name}.log`), (tab.log ?? []).join('\n'));
    // A capture waits for the tab's next frame, which a stopped tab never
    // draws: a picture missing from the report, not a run that never ends.
    const shot = await Promise.race([tab.screenshot().catch(() => null), sleep(stepLimit(2)).then(() => null)]);
    if (shot) writeFileSync(join(out, `${name}.png`), shot);
    report[`${name}Text`] = await pageText(tab);
    // Where the browser says the tab is, and whether the page answers at all:
    // a tab mid-navigation or wedged answers nothing the reads below can show.
    report[`${name}At`] = await browser.send('Target.getTargetInfo', {targetId: tab.targetId})
      .then(reply => reply.result?.targetInfo?.url ?? reply.error?.message, error => error.message).catch(() => '');
    report[`${name}Answers`] = await read(tab, 'document.readyState');
    // What the page kept of the workspaces it entered and the hosts it is a
    // member of: which way it took in on its next load.
    report[`${name}People`] = await read(tab, `JSON.stringify(${peopleOf})`);
    // Whose channels the page holds keys for: a page without its workspace's
    // keys joins no live session, so it hears nobody and nobody hears it.
    report[`${name}Channels`] = await read(tab, `globalThis.tiloChannels?.() ?? ''`);
    // What the page shows of the workspace: one app at a time, so what a
    // window would have said is the page's own text.
    report[`${name}Layout`] = await read(tab, `(document.getElementById('tilo-page')?.innerText ?? '').slice(0, 6000)`);
    report[`${name}Kept`] = await read(tab, `JSON.stringify(Object.fromEntries(Object.keys(localStorage)
      .filter(key => /workspace|membership|entered/i.test(key)).map(key => [key, localStorage.getItem(key)])))`);
  }
  writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
  for (const browser of browsers) await browser.close().catch(() => {});
  if (message) {
    for (const {name} of tabs) console.error(`--- ${name} at ${report[`${name}At`]}, answering ${report[`${name}Answers`]}\n--- ${name} kept: ${report[`${name}Kept`]}\n--- ${name} people: ${report[`${name}People`]}\n--- ${name} channels: ${report[`${name}Channels`]}\n--- ${name} layout: ${report[`${name}Layout`]}`);
    console.error(`FAIL: ${message}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const fail = message => finish(1, message);

// A read of a tab that answers at once or is said not to: a renderer that
// stopped never replies, and the protocol's own limit is two minutes.
const briefly = (tab, expression, otherwise) => Promise.race([
  tab.evaluate(expression, {timeout: 5_000}).catch(error => `${otherwise}: ${error.message}`),
  sleep(10_000).then(() => `${otherwise}: no reply in 10 s`),
]);
const pageText = tab => briefly(tab, `(document.body.innerText || '').replace(/\\s+/g, ' ').trim()`, '(the page did not answer)');
// Wait until `expression` holds in the page, or fail naming what never came.
// The page does the waiting: it reads the expression again on each change it
// shows, and the wait carries on in the next document after a navigation.
const until = async (tab, expression, what, timeout = stepLimit(3)) => {
  const value = await tab.waitFor(expression, {timeout}).catch(() => false);
  if (value) return value;
  await fail(`${what} never happened: ${(await pageText(tab)).slice(0, 600)}`);
};

// The computer the agent runs on: its own home, and the MCP configuration the
// agent keeps there.
const computer = join(out, 'computer');
mkdirSync(join(computer, 'bin'), {recursive: true});
const agentName = 'Reviewer';
// The tool the agent runs in, as `tilo login` finds it: its own command adds Tilo's MCP server,
// which it records, and writes the configuration it would keep.
const claudeCalls = join(out, 'claude-calls.txt');
const claudeConfig = join(computer, '.claude.json');
const claude = `#!/bin/sh
echo "$@" >> ${JSON.stringify(claudeCalls)}
if [ "${'$'}1" = "mcp" ] && [ "${'$'}2" = "add" ]; then
  command=""
  rest=$(echo "$@" | sed 's/.* -- //')
  program=${'$'}{rest%% *}
  printf '{"mcpServers":{"tilo":{"type":"stdio","command":"%s","args":["mcp"],"env":{}}}}\n' "${'$'}program" > ${JSON.stringify(claudeConfig)}
fi
exit 0
`;
writeFileSync(join(computer, 'bin', 'claude'), claude);
chmodSync(join(computer, 'bin', 'claude'), 0o755);
const env = {
  PATH: `${join(computer, 'bin')}:${process.env.PATH}`,
  HOME: computer,
  XDG_STATE_HOME: join(computer, 'state'),
  XDG_CONFIG_HOME: join(computer, 'config'),
  // The tool this computer's commands run under, as every tool names itself to the commands it
  // runs.
  CLAUDECODE: '1',
  // The computer's peer logs each window it shows on the person's layout.
  RUST_LOG: 'tilo_runtime::mcp::agent::link=debug,warn',
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
  const tab = await browser.tab({firstScreen: true});
  tabs.push({name, tab, browser});
  // The tab's whole console and every request that failed, beside the
  // errors the harness keeps: a step that stops leaves what led to it.
  tab.log ??= [];
  const asked = new Map();
  // Where the tab went and why: each navigation it was asked for (and by
  // what), each it took, a dialog that would hold one, and a renderer that
  // died. A tab that stops answering leaves which of these came last.
  const visitedAt = Date.now();
  const at = () => `${((Date.now() - visitedAt) / 1000).toFixed(1)}s`;
  const address = url => String(url ?? '').replace(/([?&](pair|invite)=)[^&#]*/g, '$1…').slice(0, 300);
  browser.on(({sessionId, method, params}) => {
    if (sessionId !== tab.sessionId) return;
    if (method === 'Network.requestWillBeSent') {
      asked.set(params.requestId, params.request.url);
      if (params.type === 'Document') tab.log.push(`${at()} PAGE asked for ${address(params.request.url)}`);
    } else if (method === 'Network.loadingFailed') tab.log.push(`${at()} NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}`);
    else if (method === 'Page.frameRequestedNavigation' && params.frameId === tab.targetId) {
      tab.log.push(`${at()} PAGE navigation requested (${params.reason}, ${params.disposition}) to ${address(params.url)}`);
    } else if (method === 'Page.frameNavigated' && !params.frame.parentId) tab.log.push(`${at()} PAGE now at ${address(params.frame.url)}`);
    else if (method === 'Page.navigatedWithinDocument') tab.log.push(`${at()} PAGE moved within to ${address(params.url)}`);
    else if (method === 'Page.javascriptDialogOpening') tab.log.push(`${at()} PAGE dialog ${params.type}: ${params.message}`);
    else if (method === 'Inspector.targetCrashed') {
      tab.log.push(`${at()} PAGE the renderer crashed; just before, ${lastMemory}`);
      tab.log.push(`${at()} PAGE memory now: ${browserMemory()}`);
      // The browser reports a renderer's end, and how it ended, a moment
      // after the page's own event.
      setTimeout(() => tab.log.push(`${at()} PAGE why: ${tab.crash() ?? 'the browser gave no reason'}`), 1_500);
      setTimeout(() => tab.log.push(`${at()} PAGE the browser said:\n${browser.crashLines() || '(nothing)'}`), 1000);
    }
    else if (method === 'Page.loadEventFired') tab.log.push(`${at()} PAGE loaded`);
    else if (method === 'Runtime.consoleAPICalled' && !['error', 'warning'].includes(params.type)) {
      tab.log.push(`${params.type.toUpperCase()} ${params.args.map(a => a.value ?? a.description ?? '').join(' ')}`.slice(0, 600));
    }
  });
  // The page's own heap and the browser's processes, every half second, so the sample from
  // just before a crash is in the log; and before every navigation the page asks for.
  let lastMemory = 'nothing sampled';
  const sample = async () => {
    const heap = await tab.evaluate('JSON.stringify(performance.memory ? {usedJSHeapMiB: Math.round(performance.memory.usedJSHeapSize / 1048576), totalJSHeapMiB: Math.round(performance.memory.totalJSHeapSize / 1048576)} : null)', {timeout: 400})
      .catch(() => 'no answer');
    lastMemory = `at ${at()}: page heap ${heap}; ${browserMemory()}`;
  };
  let sampling = false;
  const sampler = setInterval(() => {
    if (sampling) return;
    sampling = true;
    sample().catch(() => {}).finally(() => { sampling = false; });
  }, 500);
  sampler.unref?.();
  browser.on(({sessionId, method}) => {
    if (sessionId !== tab.sessionId || method !== 'Page.frameRequestedNavigation') return;
    tab.log.push(`${at()} PAGE memory before it: ${lastMemory}`);
  });
  await tab.send('Network.enable', {}).catch(() => {});
  await tab.send('Inspector.enable', {}).catch(() => {});
  await tab.send('Emulation.setDeviceMetricsOverride', shellMetrics).catch(() => {});
  await tab.send('Browser.grantPermissions', {origin: new URL(site).origin,
    permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  await tab.navigate(site);
  // The join behind the held first screen founds this browser's person: the
  // entry shows their 24 recovery words once, and the first screen waits for
  // them to be kept.
  await keepRecoveryWords(tab, `${name}: founded`);
  await until(tab, `document.documentElement.dataset.firstScreen !== undefined && !!document.getElementById('first-screen-agent')`,
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
// What is on the clipboard now, read in the tab in front as the clipboard requires.
const clipboardNow = tab => tab.evaluate(`navigator.clipboard.readText().catch(() => '')`).catch(() => '');
// What the page put on the clipboard, once it did. The page writes it inside
// the click and the browser lands the write a moment later, and the
// clipboard is the browser's, shared by every tab: a prompt an earlier copy
// left there is not this one, so the read waits for the clipboard to change
// from what it held before the click (`before`).
const copied = async (tab, what, before) => {
  const read = `navigator.clipboard.readText().then(text =>
    text.match(/(?:tilo|sh -s) (?:login|device add) /) && text !== ${JSON.stringify(before)} ? text : '', () => '')`;
  // The clipboard is the browser's, not the page's: a write landing is no
  // change the page tells anyone about, so this is the one wait here that
  // reads on a clock, and it holds a step.
  for (const deadline = Date.now() + stepLimit(3); Date.now() < deadline;) {
    const text = await tab.evaluate(read).catch(() => '');
    if (text) return text;
    await sleep(200);
  }
  await fail(`${what} never reached the clipboard`);
};
// What a prompt names: the ticket this tab's host served (`tilo1…`), and, from a host still
// handing out login codes, the short code it names instead.
const commandOf = prompt => {
  const login = prompt.match(/(?:tilo|sh -s) login (\S+)/);
  const add = prompt.match(/(?:tilo|sh -s) device add (\S+)/);
  const text = login?.[1] ?? add?.[1];
  if (!text) return null;
  return {text, kind: add ? 'device add' : 'login',
    ticket: /^tilo1[0-9a-f]+$/.test(text) ? text : null,
    code: /^[2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4}$/.test(text) ? text : null,
    site: prompt.match(/--site (\S+?)(?:,|\s|$)/)?.[1] ?? null};
};
// The steps the tab shows, as it shows them: each line the list ever held,
// kept in the page the moment the list changes. Read by polling, a step the
// page drew and replaced (or the screen left with) between two reads was
// never seen; and the list, hidden once the screen says the agent is in,
// reads as one run-together line.
const followSteps = async (tab, done) => {
  await tab.evaluate(`(() => {
    const seen = globalThis.__checkSteps ??= [];
    const read = () => {
      for (const item of document.querySelectorAll('#first-screen-steps li')) {
        const line = item.textContent.trim();
        if (line && !seen.includes(line)) seen.push(line);
      }
    };
    globalThis.__checkStepsWatch ??= new MutationObserver(read);
    globalThis.__checkStepsWatch.observe(document.documentElement, {subtree: true, childList: true, characterData: true});
    read();
  })()`).catch(error => fail(`the tab's steps could not be followed: ${error.message}`));
  // The watch above keeps every line the list held; the wait ends when the
  // screen says it is done, or a step's worth of steps has gone by.
  await tab.waitFor(`!!(${done})`, {timeout: stepLimit(4)}).catch(() => {});
  return tab.evaluate('globalThis.__checkSteps ?? []').catch(() => []);
};
// The person the page is, by their root, and the workspace in front.
/** Everyone the page shows as being here, read where the page shows them: a
 * desktop's mount, or the people view the server renders, which lists each
 * person with what they are doing. */
const peopleOf = `[...document.querySelectorAll('#tilo-page .tilo-person')].map(row => ({
  label: row.querySelector('.tilo-person-name')?.textContent ?? '',
  doing: row.querySelector('.tilo-person-what')?.textContent ?? null,
  here: row.hasAttribute('data-present'),
}))`;

// The person is who the page keeps its memberships as, on the server's host.
const person = tab => tab.evaluate(`JSON.stringify({
  user: (() => { try {
    const all = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}');
    return all[${JSON.stringify(new URL(server).origin)}]?.userPublicKey ?? null;
  } catch { return null; } })(),
  title: document.title,
  workspace: (() => { try { return JSON.parse(globalThis.localStorage.getItem('tilo-chosen-workspace') ?? 'null')?.workspace ?? null; } catch { return null; } })(),
  people: (() => { try { return [${peopleOf}][0]; } catch { return null; } })(),
})`).then(JSON.parse).catch(() => ({}));

// 1. Set up through the agent.
const first = await visit('setup');
mark('the first browser opened the site');
// A browser the site knows nothing of makes nothing for the person while
// they are still deciding: its button offers the prompt, and the press makes
// the code — and with it, on the host, the workspace the code names. The
// press after that copies exactly the text the screen shows.
const visited = Date.now();
await until(first, `document.getElementById('first-screen')?.dataset.setup === undefined
  && !!document.getElementById('first-screen-agent')`, 'the first screen offering to set up');
await click(first, 'first-screen-agent');
await until(first, `document.getElementById('first-screen')?.dataset.setup === 'ready'`, 'the press making the setup prompt');
report.promptShownMs = Date.now() - visited;
const shown = await first.evaluate(`document.getElementById('first-screen-offer').textContent`);
const beforeSetup = await clipboardNow(first);
await click(first, 'first-screen-agent');
await until(first, `document.documentElement.dataset.firstScreen === 'waiting'`, 'the first screen answering the click on Set up');
const prompt = await copied(first, 'the setup prompt', beforeSetup);
if (prompt !== shown) await fail(`the click copied something else than the prompt shown: ${prompt} / ${shown}`);
report.prompt = prompt;
mark('the page copied its setup prompt');
const setup = commandOf(prompt);
if (!setup) await fail(`the prompt names no tilo login ticket: ${prompt}`);
if (!/^Set up Tilo for me: run this command, then do what it tells you: curl -fsSL \S+ \| sh -s login tilo1[0-9a-f]+$/.test(prompt)) {
  await fail(`the prompt is not the one-line setup naming the ticket: ${prompt}`);
}
const login = tilo(['login', setup.ticket, '--agent', agentName]);
// The tab's steps are followed while the agent goes: the screen leaves only
// once the agent has confirmed Tilo's tools in its own configuration.
const following = followSteps(first, `document.documentElement.dataset.firstScreen === undefined || document.documentElement.dataset.firstScreen === 'connected'`);
const loggedIn = await login;
report.login = loggedIn;
if (loggedIn.code !== 0) await fail(`tilo login failed (${loggedIn.code}): ${loggedIn.stderr}${loggedIn.stdout}`);
if (!/restart/i.test(loggedIn.stdout) || !/\btilo_[a-z]+\b/.test(loggedIn.stdout)) {
  await fail(`tilo login did not tell the agent to restart and which tool to call: ${loggedIn.stdout}`);
}
if (!/holds the MCP server tilo/.test(loggedIn.stdout) || !/tilo_[a-z]+/.test(loggedIn.stdout)) {
  await fail(`tilo login did not confirm Tilo's tools in the agent's own configuration: ${loggedIn.stdout}`);
}
if (/failed/i.test(loggedIn.stdout)) await fail(`tilo login said something failed: ${loggedIn.stdout}`);
// The tool's own command added the server, as the login runs it, and the configuration it keeps
// holds Tilo's server: what the tab shows the tools added is what the agent's own file holds.
const calls = readFileSync(claudeCalls, 'utf8');
report.claudeCalls = calls;
if (!/^mcp add --scope user tilo -- \S*tilo mcp$/m.test(calls)) {
  await fail(`the tool's own command never added Tilo's server: ${JSON.stringify(calls)}`);
}
const kept = readFileSync(claudeConfig, 'utf8');
report.claudeConfig = kept;
if (!JSON.parse(kept)?.mcpServers?.tilo?.command?.endsWith('tilo')) {
  await fail(`the tool's own configuration holds no Tilo server: ${kept}`);
}
const steps = await following;
report.steps = steps;
if (steps.length < 2) await fail(`the tab showed no steps as the agent went: ${JSON.stringify(steps)}`);
// The tools the tab says are added are the ones the agent's check found in
// its own configuration; a step the command did not report is never claimed.
if (!steps.some(line => line.includes(`Tilo’s tools are added to ${agentName}`))) {
  await fail(`the tab never showed Tilo's tools added, though the agent's check confirmed them: ${JSON.stringify(steps)}`);
}

// The screen leaves for the person's own workspace.
await until(first, `document.documentElement.dataset.firstScreen === undefined`, 'the first screen leaving', stepLimit(2));
// The person's own workspace, drawn, as the page publishes it: the shell its
// host admitted it to is presented, and the tab is titled after that
// workspace, never the Lounge.
const ownWorkspace = `(() => {
  // The page names its workspace where it says it arrived, and is titled
  // after the workspace when it shows the workspace itself.
  const name = document.documentElement.dataset.workspace ?? '';
  const drawn = document.documentElement.dataset.arrival === 'done' && !!document.querySelector('#tilo-page [data-p]');
  return drawn && name && !/lounge/i.test(name) ? name : '';
})()`;
report.workspace = await until(first, ownWorkspace, "the person's own workspace drawn", stepLimit(2));
mark('the agent logged in and the tab is in the person\'s own workspace');

// The agent is present once its MCP server connects, as the agent starts it
// once restarted, and what it does shows beside its name: one tool call, said
// as the workspace says it ("Looking at the apps" for a look at its apps).
// Who is here is live presence alone: the agent's own, on the workspace's
// live session.
const doing = 'Looking at the apps';
// The agent's MCP server stays up as a restarted agent keeps it, while the
// tab is looked at; its answers are kept for the report.
const mcp = spawn(tiloBin, ['mcp'], {env, stdio: ['pipe', 'pipe', 'pipe']});
let answers = '';
mcp.stdout.on('data', chunk => { answers += chunk; report.mcp = answers.slice(-2000); });
mcp.stderr.on('data', chunk => { answers += chunk; report.mcp = answers.slice(-2000); });
for (const line of [
  {jsonrpc: '2.0', id: 1, method: 'initialize', params: {protocolVersion: '2025-06-18', capabilities: {}, clientInfo: {name: 'check', version: '1'}}},
  {jsonrpc: '2.0', method: 'notifications/initialized'},
  {jsonrpc: '2.0', id: 2, method: 'tools/call', params: {name: 'tilo_ls', arguments: {}}},
]) mcp.stdin.write(`${JSON.stringify(line)}\n`);
const beside = `(() => { const row = (${peopleOf}).find(person => person.label?.includes(agentName)); return !!row && JSON.stringify(row).includes(${JSON.stringify(doing)}); })()`;
// Its MCP server's own answers, on a failure: whether the tool call was
// answered tells a slow presence from a call that never ran.
if (!await first.waitFor(beside, {timeout: stepLimit(3)}).catch(() => false)) {
  await fail(`${agentName} present, with "${doing}" beside its name never happened; its MCP server said: ${answers.slice(-1500) || 'nothing'}; the page: ${(await pageText(first)).slice(0, 400)}`);
}
report.mcp = answers.slice(0, 2000);
const signedUp = await person(first);
mark('the agent is present beside its name');
report.person = signedUp;

// A second agent on the same computer (another window of the same agent, say) starts
// its own `tilo mcp` while the first stays connected: both are the one device
// `tilo login` made, and neither cuts the other off. It does what the login's
// words tell it to, and the person's tab shows the note it writes.
const otherAgent = spawn(tiloBin, ['mcp'], {env, stdio: ['pipe', 'pipe', 'pipe']});
const secondAnswers = new Map();
let secondOut = '';
otherAgent.stdout.on('data', chunk => {
  secondOut += chunk;
  let at;
  while ((at = secondOut.indexOf('\n')) >= 0) {
    const line = secondOut.slice(0, at);
    secondOut = secondOut.slice(at + 1);
    try { const answer = JSON.parse(line); secondAnswers.get(answer.id)?.(answer); } catch {}
  }
});
let secondId = 0;
const ask = (method, params) => new Promise(resolve => {
  const id = ++secondId;
  secondAnswers.set(id, resolve);
  otherAgent.stdin.write(`${JSON.stringify({jsonrpc: '2.0', id, method, params})}\n`);
});
const call = async (name, args) => {
  const answer = await Promise.race([
    ask('tools/call', {name, arguments: args}),
    sleep(stepLimit(4)).then(() => ({error: {message: 'no answer'}})),
  ]);
  const said = (answer.result?.content ?? []).map(block => block.text).join('\n') || JSON.stringify(answer);
  (report.secondAgent ??= []).push({name, args, said: said.slice(0, 400), link: (said.match(/link: \S+/) ?? [null])[0], notShown: (said.match(/not shown on the person's layout: .*/) ?? [null])[0]});
  if (answer.error || answer.result?.isError) await fail(`the second agent's ${name} ${JSON.stringify(args)} failed: ${said}`);
  return said;
};
await ask('initialize', {protocolVersion: '2025-06-18', capabilities: {}, clientInfo: {name: 'check-second', version: '1'}});
otherAgent.stdin.write(`${JSON.stringify({jsonrpc: '2.0', method: 'notifications/initialized'})}\n`);
const noteTitle = 'Hello from the second agent';
await call('tilo_do', {path: 'tasks#open'});
await call('tilo_do', {path: 'tasks#create', args: {title: noteTitle}});
// The first agent still acts after the second did: it lists the notes, the
// second agent's among them.
const firstAnswer = new Promise(resolve => {
  let seen = '';
  mcp.stdout.on('data', chunk => { seen += chunk; if (seen.includes('"id":3')) resolve(seen); });
});
mcp.stdin.write(`${JSON.stringify({jsonrpc: '2.0', id: 3, method: 'tools/call', params: {name: 'tilo_ls', arguments: {path: 'tasks/tasks.issues'}}})}\n`);
const firstSaid = await Promise.race([firstAnswer, sleep(stepLimit(4)).then(() => 'no answer')]);
report.firstAfterSecond = firstSaid.slice(-600);
if (!firstSaid.includes(noteTitle)) await fail(`the first agent no longer reads the workspace after the second acted: ${firstSaid.slice(-600)}`);
// The window the second agent's open put on the workspace's shared desktop
// reaches the person's page as a window of Notes, and its content shows the
// note it wrote. On a failure, what the page holds: its text, and the address it is at, which
// tells a page that never opened from one showing nothing.
const windowsHeld = `JSON.stringify({
  page: document.getElementById('tilo-page')?.innerText?.slice(0, 600) ?? null,
  at: location.pathname,
})`;
// The page the server renders shows one app at a time: the note the second
// agent wrote is read where Notes lists it, at the app's own address.
const pageWorkspace = await first.evaluate(`document.getElementById('tilo-page') ? (location.pathname.split('/')[2] ?? '') : ''`).catch(() => '');
await first.navigate(`${site}/w/${pageWorkspace}/notes`);
const opened = await first.waitFor(`[...document.querySelectorAll('#tilo-page [data-a=activate][data-t^="note-"]')]
  .some(row => (row.innerText || '').includes(${JSON.stringify(noteTitle)}))`, {timeout: stepLimit(4)}).then(() => 'shown').catch(() => '');
if (!opened) await fail(`the person's tab showing the note the second agent opened never happened: ${await briefly(first, windowsHeld, '(the page did not answer)')}; the second agent was answered: ${JSON.stringify(report.secondAgent ?? []).slice(0, 1500)}`);
const windowText = `(document.getElementById('tilo-page')?.innerText ?? '')`;
report.noteWindow = await first.evaluate(`(${windowText}).replace(/\s+/g, ' ').slice(0, 2000)`)
  .catch(() => '(the tab did not answer)');
await until(first, `(${windowText}).includes(${JSON.stringify(noteTitle)})`, 'the person\'s tab showing the second agent\'s note', stepLimit(3));
otherAgent.stdin.end();
mark('a second agent on the computer wrote a note the tab shows, the first still connected');

// Loaded again while the agent's MCP server is still connected, the tab is the
// same person in the same workspace, titled after it, with the agent still
// there. The browser takes the reload before the new document answers, and the
// document being replaced keeps answering until it does: a tab read then is the
// page that is going away, titled "" rather than after its workspace. So the
// reload is waited out on the new document's own shell (waits.mjs, `reloaded`)
// before anything is read of the tab.
const how = await reloaded(first);
if (how !== 'presented') {
  await fail(`the reloaded tab never came back (${how}): ${(await pageText(first)).slice(0, 600)}`);
}
// Who is here is what the workspace's page shows: on the page the server
// renders, the workspace's people, read there; on a desktop, its mount.
if (pageWorkspace) await first.navigate(`${site}/w/${pageWorkspace}`);
await until(first, `(document.querySelector('#tilo-page .tilo-people')?.innerText ?? '').includes(${JSON.stringify(agentName)})`,
  `${agentName} present in the workspace after a reload`, stepLimit(2));
mcp.stdin.end();
const after = await person(first);
mark('the tab reloaded as the same person, with the agent still there');
report.reloaded = after;
if (!signedUp.user || after.user !== signedUp.user) {
  await fail(`after a reload the tab is ${after.user ?? 'nobody'}, not the person who set up (${signedUp.user ?? 'unknown'})`);
}
if (!signedUp.workspace || after.workspace !== signedUp.workspace) {
  await fail(`after a reload the tab is in ${after.workspace ?? 'no workspace'}, not the person's own (${signedUp.workspace ?? 'unknown'})`);
}
if (/lounge/i.test(after.title) || after.title !== signedUp.title) {
  await fail(`after a reload the tab is titled "${after.title}", not "${signedUp.title}"`);
}
// The reloaded tab opens its layout from the copy its profile kept of the workspace's, and shows
// that workspace with the Notes window still on it.
await first.navigate(`${site}/w/${pageWorkspace}/notes`);
const reopened = await first.waitFor(`[...document.querySelectorAll('#tilo-page [data-a=activate][data-t^="note-"]')]
  .some(row => (row.innerText || '').includes(${JSON.stringify(noteTitle)}))`, {timeout: stepLimit(4)})
  .then(() => 'shown').catch(() => '');
if (!reopened) await fail(`after a reload the note the second agent wrote is gone: ${await briefly(first, windowsHeld, '(the page did not answer)')}; the tab says: ${(await pageText(first)).slice(0, 600)}`);
mark('the reloaded tab still shows the note the second agent wrote');

// 2. Back on a browser that keeps nothing: signed in through the agent.
const second = await visit('returning');
mark('a browser that keeps nothing opened the site');
await until(second, `document.getElementById('first-screen')?.dataset.signIn === 'ready'`, 'the sign-in prompt made');
const beforeSignIn = await clipboardNow(second);
await click(second, 'first-screen-agent-sign-in');
await until(second, `document.documentElement.dataset.firstScreen === 'waiting'`, 'the first screen answering the click on Sign in with your agent');
const again = await copied(second, 'the sign-in prompt', beforeSignIn);
report.signInPrompt = again;
const signIn = commandOf(again);
if (!signIn) await fail(`the sign-in prompt names no ticket or code: ${again}`);
const approved = await tilo(signIn.kind === 'device add'
  ? ['device', 'add', signIn.ticket]
  : ['login', signIn.code, ...(signIn.site ? ['--site', signIn.site] : [])]);
report.approve = approved;
if (approved.code !== 0) await fail(`tilo login did not approve the tab (${approved.code}): ${approved.stderr}${approved.stdout}`);
// The person's root stays in the tab that set it up, so the computer that ran `tilo device add`
// holds none: it hands the signing on, and the first tab, which holds the root, asks about the
// browser it would sign in and approves it.
if (signIn.kind === 'device add') {
  if (!/handed the signing on/.test(approved.stdout)) {
    await fail(`tilo device add did not hand the signing on from a computer with no root: ${approved.stdout}${approved.stderr}`);
  }
  const askedAbout = await until(first, `(() => {
    const bar = document.getElementById('tilo-sign-in');
    return bar && !bar.hidden ? bar.innerText.trim() : '';
  })()`, 'the first tab asking about the browser signing in', stepLimit(8));
  report.signInAsked = askedAbout;
  if (!/becomes one of your devices/.test(askedAbout)) {
    await fail(`the first tab's ask names no device of the person's: ${askedAbout}`);
  }
  await click(first, 'tilo-sign-in-allow');
  mark('the first tab approved the browser signing in');
}
// The person asked their agent to sign them in on this tab, so the tab asks
// them nothing about whose device it becomes: that card, which warns of a
// link someone else made, is for a link handed to a device.
const signedInOrAsked = await until(second, `(() => {
    if (document.querySelector('[data-card="pair-whose"]')) return 'asked';
    if (document.documentElement.dataset.firstScreen !== undefined) return false;
    return document.documentElement.dataset.arrival === 'done' && !!document.querySelector('#tilo-page [data-p]');
  })()`, 'the returning tab signed in', stepLimit(4));
if (signedInOrAsked === 'asked') {
  await fail('the tab its person signed in with their agent asked whose device it becomes, as for a link someone else made');
}
const back = await person(second);
report.returned = back;
if (!signedUp.user || back.user !== signedUp.user) {
  await fail(`the returning tab is ${back.user ?? 'nobody'}, not the person who set up (${signedUp.user ?? 'unknown'})`);
}
// Signed in, it opens the person's own workspace, as the first tab did.
await until(second, `!/lounge/i.test(document.documentElement.dataset.workspace ?? '')`,
  'the returning tab in the person\'s own workspace, not the Lounge', stepLimit(2));
mark('the returning tab signed in as the person, in their own workspace');
await finish(0);
