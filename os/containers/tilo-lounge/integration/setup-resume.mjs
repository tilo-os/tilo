// A person's setup survives whatever their browser does mid-way: through the
// real page, the Lounge's relay and the `tilo` command an agent runs, every
// step of "Set up with your agent" is reloaded, closed and reopened, gone
// back and forward to, and opened in a second tab, and each time the page
// must show the step the person was at (at its first frame, from what this
// browser kept, before the host answers), then carry on from there. It must
// never drop the person into the site's home workspace (the Lounge) by
// surprise: the first screen joins the Lounge behind it, which is exactly
// what used to make a reload skip the setup.
//
// The matrix:
//   1. before asking: nothing is made while the person is still deciding; the
//      press makes the prompt, and a reload after the Lounge let the tab in
//      shows the start, with the same prompt;
//   2. after copy: a reload, back and forward, a second tab, and a closed and
//      reopened tab show "Paste it into your agent" with the same prompt;
//   3. agent installing: the agent runs `tilo login` while the tab is closed,
//      and reopening it approves the agent and follows it;
//   4. agent signed in: a reload once it logged in shows what it did so far;
//   5. entered: the tab lands in the person's own workspace, and a reload,
//      back and forward land there again;
//   6. agent finishes while the tab is closed: reopening says "Reviewer is in
//      your workspace" and opens it;
//   7. code expiry: a code that ran out while the tab was closed shows a fresh
//      one with a note, and copying it follows the fresh one;
//   8. offline, then online: the screen says it is offline, then carries on;
//   9. an agent that runs `tilo login` the moment the prompt shows, before it
//      copied, is waited for, never refused, and gets in.
//
// usage: node setup-resume.mjs SITE_URL SERVER_ORIGIN TILO_BIN OUT_DIR
//
// Saves a capture of each resumed state and a JSON report to OUT_DIR; exits
// non-zero when any case lands on the wrong screen.
import {execFile} from 'node:child_process';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {stepLimit, shellMetrics, machine, keepRecoveryWords} from './waits.mjs';
import {clock} from './timing.mjs';
import {agentCommand} from './shell.mjs';

const [site, server, tiloBin, out] = process.argv.slice(2);
if (!site || !server || !tiloBin || !out) {
  console.error('usage: node setup-resume.mjs SITE_URL SERVER_ORIGIN TILO_BIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const report = {site, server, at: new Date().toISOString(), cases: []};
const failures = [];
const open = [];
// Each numbered case says how long it took as the next begins, the line
// named after the case that ended, and each page load and agent stage inside
// it says its own seconds, so a slow run names the part that was slow.
const mark = clock();
let caseNo = null;
const check = (ok, what, detail = '') => {
  const number = what.match(/^(\d+)\./)?.[1] ?? caseNo;
  if (number !== caseNo) {
    if (caseNo !== null) mark(`case ${caseNo}`);
    caseNo = number;
  }
  report.cases.push({ok: Boolean(ok), what, ...(detail ? {detail} : {})});
  console.log(`${ok ? 'ok' : 'FAIL'} ${what}${!ok && detail ? `: ${detail}` : ''}${ok ? '' : ` (the machine: ${machine().text})`}`);
  if (!ok) {
    failures.push(what);
    // A failed check keeps no outputs: what the tab in front said last is
    // said here, where the run's log keeps it.
    let said = [];
    try { said = (tab?.log ?? []).slice(-25); } catch { /* no tab yet */ }
    if (said.length) console.log(`--- the tab said, last:\n${said.join('\n').slice(0, 6000)}`);
  }
  return Boolean(ok);
};

// The flow drives two tabs of one browser and reads the one behind: a setup
// is one, whichever tab shows it, and a tab that went back says so to the
// other through storage. The browser's baseline leaves every tab answering
// as the front one does (`launch` in tools/web/chrome.mjs).
const browser = await launch();
const finish = async () => {
  mark();
  await Promise.race([Promise.all(pictures), sleep(stepLimit(1))]);
  for (const {name, tab} of open) {
    writeFileSync(join(out, `${name}.log`), (tab.log ?? []).join('\n'));
  }
  report.failures = failures;
  writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
  await browser.close().catch(() => {});
  if (failures.length) console.error(`FAIL: ${failures.length} case(s): ${failures.join('; ')}`);
  process.exit(failures.length ? 1 : 0);
};
process.on('unhandledRejection', async error => { check(false, 'the run went on', String(error?.stack ?? error)); await finish(); });

// What the page showed at its first frame with its markup in: the step the
// first screen opened on and what it drew, at the first animation frame
// after the page's body is parsed past the screen and the paint the entry
// makes of it (its status line is the marker), before any module of the page
// has run. The page the entry hands over has its own element where the
// screen was.
const FIRST_FRAME = `(() => {
  const look = () => {
    if (!document.getElementById('bar') && !document.getElementById('tilo-page')) { requestAnimationFrame(look); return; }
    const text = id => document.getElementById(id)?.textContent ?? null;
    const expired = document.querySelector('.fr-expired');
    globalThis.__firstFrame = {step: document.documentElement.dataset.firstScreen ?? null, offer: text('first-screen-offer'),
      prompt: text('first-screen-prompt'), steps: text('first-screen-steps'), heading: text('first-screen-connected'),
      net: text('first-screen-net'), expired: !!expired && getComputedStyle(expired).display !== 'none',
      modules: !!globalThis.__tiloFirstScreenReady, held: document.documentElement.hasAttribute('data-first-screen-hold')};
  };
  requestAnimationFrame(look);
})();`;

// A tab of one browser profile (`context`), its first screen not marked seen.
const tabIn = async (context, name) => {
  const tab = await browser.tab({context, firstScreen: true});
  open.push({name, tab});
  await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: FIRST_FRAME});
  // Each tab counts as focused, as the one a person works in: the clipboard
  // answers only a focused page, and several tabs share this browser.
  await tab.send('Emulation.setFocusEmulationEnabled', {enabled: true}).catch(() => {});
  await tab.send('Emulation.setDeviceMetricsOverride', shellMetrics);
  await tab.send('Browser.grantPermissions', {origin: new URL(site).origin, browserContextId: context,
    permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  return tab;
};
// A picture of the tab at this step, for the report. It is the tab's next
// frame after the step, and a busy machine may take many seconds to draw it,
// so the run asks for it and goes on: a picture shows the first frame after
// its step, and a frame that never comes is a picture missing from the report
// (said in its line), never seconds of the run. The run's end waits one step
// for the pictures still owed.
const pictures = [];
const shot = (tab, name, at = caseNo) => {
  report.captures = [...(report.captures ?? []), `${name}.png`];
  const asked = Date.now();
  const taking = sleep(400).then(() => tab.screenshot()).catch(() => null).then(async png => {
    if (png) writeFileSync(join(out, `${name}.png`), png);
    console.log(`picture ${name} (case ${at}): ${png ? `drawn ${((Date.now() - asked) / 1000).toFixed(1)} s after its step` : 'none: the tab drew no frame before it closed or the run ended'}`);
    // Where a check's outputs cannot be fetched, the log carries the capture
    // too, small, when asked (SETUP_RESUME_LOG_SHOTS=1).
    if (png && process.env.SETUP_RESUME_LOG_SHOTS === '1') {
      const jpeg = await tab.send('Page.captureScreenshot', {format: 'jpeg', quality: 55}).catch(() => null);
      // In pieces: a log line is cut after 16 KiB.
      const data = jpeg?.result?.data ?? '';
      const pieces = Math.ceil(data.length / 12_000);
      for (let piece = 0; piece < pieces; piece += 1) {
        console.log(`SHOT ${name} ${piece + 1}/${pieces} ${data.slice(piece * 12_000, (piece + 1) * 12_000)}`);
      }
    }
  });
  pictures.push(taking);
  owed.set(tab, [...(owed.get(tab) ?? []), taking]);
};
// A tab closed once the pictures asked of it are drawn, or a step has passed.
const owed = new Map();
const close = async tab => {
  await Promise.race([Promise.all(owed.get(tab) ?? []), sleep(stepLimit(1))]);
  await tab.close();
};
// Wait until `expression` holds in the page, or answer null: the page reads
// the expression again on each change it shows, and the wait carries on in the
// next document when a reload or a step back replaces this one.
const until = async (tab, expression, timeout = stepLimit(3)) =>
  tab.waitFor(expression, {timeout}).catch(() => null);
const firstFrame = tab => until(tab, 'globalThis.__firstFrame && JSON.stringify(globalThis.__firstFrame)', stepLimit(2))
  .then(text => { mark(`case ${caseNo ?? 1}: a page's first frame`); return JSON.parse(text ?? '{}'); });
const setup = tab => tab.evaluate(`localStorage.getItem('tilo-setup')`).then(text => JSON.parse(text ?? 'null')).catch(() => null);
const pageText = tab => tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 400)`).catch(() => '');
// Where the tab is: the first screen's step, the workspace it is titled after,
// and what its page is doing (the mount's own phase, frames and the name the
// workspace answered admission with), so a wait that runs out says which of
// them it was waiting on.
const where = tab => tab.evaluate(`(() => {
  const root = document.documentElement;
  const page = document.getElementById('tilo-page');
  return JSON.stringify({step: root.dataset.firstScreen ?? null,
    screen: root.dataset.firstScreen !== undefined, title: document.title,
    entry: root.dataset.entry ?? null, arrival: root.dataset.arrival ?? null,
    state: page?.dataset.state ?? null, refused: page?.dataset.refused ?? null,
    workspace: root.dataset.workspace ?? null, path: location.pathname});
})()`)
  .then(JSON.parse).catch(() => ({}));
const inLounge = async tab => {
  const at = await where(tab);
  return !at.screen && /lounge/i.test(at.title);
};
// The person's own workspace, drawn, as the page publishes it: the first
// screen gone, the shell its host admitted it to presented, and the tab
// titled after that workspace, never the Lounge.
// The person's own workspace, shown by the page the entry handed them to: the
// first screen gone, the page arrived and showing, and the workspace's own
// name on it — never the Lounge's.
const OWN_WORKSPACE = `(() => {
  const root = document.documentElement;
  const name = root.dataset.workspace ?? '';
  const firstScreen = root.dataset.firstScreen ?? null;
  return firstScreen === null && root.dataset.arrival === 'done'
    && document.querySelector('#tilo-page [data-p]') && name && !/lounge/i.test(name) ? name : '';
})()`;
const forget = tab => tab.evaluate('delete globalThis.__firstFrame').catch(() => {});
// The mark gone, the waits that follow read the first frame of the document
// the reload or the step back brings, never this one's.
const reload = async tab => { await forget(tab); await tab.reload(); };
// Back one entry in the tab's history, as the browser's Back button goes.
const goBack = async tab => { await forget(tab); await tab.evaluate('history.back()').catch(() => {}); };
// A person's click: on the tab in front, as only that one may use the clipboard.
// A person's click on the button `id`, once it is what stands at its centre:
// a click on a layer still over it (the screen fading in) lands on that layer
// and presses nothing.
const click = async (tab, id) => {
  await tab.front();
  const box = JSON.parse(await until(tab, `(() => { const e = document.getElementById(${JSON.stringify(id)});
    if (!e || e.disabled) return ''; const r = e.getBoundingClientRect(); if (!r.width) return '';
    const x = r.x + r.width / 2, y = r.y + r.height / 2;
    const top = document.elementFromPoint(x, y);
    if (!top || !e.contains(top)) return '';
    return JSON.stringify({x, y}); })()`) ?? 'null');
  if (!box) return false;
  for (const type of ['mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x: box.x, y: box.y, button: 'left', clickCount: 1});
  }
  return true;
};
// The clipboard's text as this page can read it: nothing where it may not.
const clipboardText = tab => tab.evaluate(
  `navigator.clipboard.readText().then(text => text, () => '')`).catch(() => '');
// The press that makes the setup prompt — a first visit makes nothing while
// the person is still deciding — and the prompt once it is on the screen. The
// button is there once the entry let the tab in; a press before then lands
// nowhere.
const makePrompt = async tab => {
  const state = await until(tab, `(() => {
    const screen = document.getElementById('first-screen');
    if (document.documentElement.dataset.firstScreen !== '' || !screen || getComputedStyle(screen).display === 'none'
      || screen.dataset.setup === 'waiting') return '';
    return screen.dataset.setup ?? 'idle';
  })()`);
  if (!state || state === 'failed') return null;
  // After Back, another tab may already have requested the next prompt. A
  // click makes one only when the screen is idle; clicking a ready one copies it.
  if (state === 'idle') await click(tab, 'first-screen-agent');
  return until(tab, `document.getElementById('first-screen')?.dataset.setup === 'ready' && document.getElementById('first-screen-offer').textContent`);
};
// What a click on Copy wrote: the prompt the clipboard holds once it no longer
// holds what it held before the click. The page writes the clipboard inside
// the click and the browser takes a moment to land it, so a read that only
// asks for a prompt returns the one an earlier copy left there; this copy's is
// the one that is not that text.
const copy = async (tab, id) => {
  // The clipboard answers only the tab in front, so read it there, as the
  // click's press needs it in front too.
  await tab.front();
  const before = await clipboardText(tab);
  await click(tab, id);
  // The clipboard is the browser's, not the page's: a write landing is no
  // change the page tells anyone about, so this is the one wait here that
  // reads on a clock, and it holds a step.
  const read = `navigator.clipboard.readText().then(text =>
    text.match(/(?:tilo|sh -s) (?:login|device add) /) && text !== ${JSON.stringify(before)} ? text : '', () => '')`;
  for (const deadline = Date.now() + stepLimit(1); Date.now() < deadline;) {
    const text = await tab.evaluate(read).catch(() => '');
    if (text) return text;
    await sleep(200);
  }
  return null;
};
const codeOf = prompt => agentCommand(prompt)?.text ?? null;
const steps = tab => tab.evaluate(`document.getElementById('first-screen-steps')?.innerText ?? ''`).catch(() => '');

// The agent's computer, as agent-setup.mjs makes it: its own home, where the
// agent keeps its MCP configuration.
const computer = name => {
  const home = join(out, `computer-${name}`);
  mkdirSync(join(home, 'bin'), {recursive: true});
  return {PATH: `${join(home, 'bin')}:${process.env.PATH}`, HOME: home, XDG_STATE_HOME: join(home, 'state'),
    XDG_CONFIG_HOME: join(home, 'config')};
};
// `tilo login TICKET --agent Reviewer` on that computer, then what its words ask
// of the agent: Tilo's server added to the agent's own configuration and
// confirmed with `tilo agent check --config`. A promise of how both ended, and
// `asked`, which resolves once the login says it waits for the person's tab,
// the moment the agent is installing in earnest.
const login = (code, env) => {
  let saidWaiting;
  const asked = new Promise(resolve => { saidWaiting = resolve; });
  const child = execFile(tiloBin, ['login', code, '--agent', 'Reviewer'], {env, timeout: 600_000, maxBuffer: 1 << 20});
  let stdout = '', stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => {
    stderr += chunk;
    if (stderr.includes('Asking your device to add this computer')) saidWaiting(true);
  });
  const loggedIn = new Promise(resolve => child.on('close', code => { saidWaiting(false); resolve({code: code ?? 1, stdout, stderr}); }));
  const ended = loggedIn.then(ran => {
    if (ran.code !== 0) return ran;
    const configuration = join(env.HOME, 'agent-mcp.json');
    writeFileSync(configuration, `${JSON.stringify({mcpServers: {tilo: {type: 'stdio', command: tiloBin, args: ['mcp'], env: {}}}})}\n`);
    return new Promise(resolve => execFile(tiloBin, ['agent', 'check', '--config', configuration], {env, timeout: 180_000, maxBuffer: 1 << 20},
      (error, out, err) => resolve({code: error ? (error.code ?? 1) : 0, stdout: `${ran.stdout}${out}`, stderr: `${ran.stderr}${err}`})));
  });
  return Object.assign(ended, {asked});
};

// ---- One person, through every step.
const person = await browser.context();
let tab = await tabIn(person, 'setup');
await tab.navigate(site);
let frame = await firstFrame(tab);
// A browser the site knows nothing of is not shown the setup ahead of its
// entry's answer (first-screen.inline.js): the screen is held at the first frame,
// and shown once the entry let the person in.
check(frame.step === null && frame.held, '1. a browser new here holds the first screen at its first frame, until the entry answers', JSON.stringify(frame));
// The join behind the held first screen founds the person: the entry shows
// their 24 recovery words once, and the screen waits for them to be kept
// before it offers to set up.
await keepRecoveryWords(tab, 'setup: founded');
// Nothing of the person's is made while they are still deciding: the button
// offers the prompt, and pressing it makes the code — and with it, on the
// host, the workspace the code names.
const offering = await until(tab, `document.documentElement.dataset.firstScreen === ''
  && getComputedStyle(document.getElementById('first-screen')).display !== 'none'`);
check(offering, '1. the first screen offers to set up', await pageText(tab));
const idle = await tab.evaluate(`document.getElementById('first-screen')?.dataset.setup === undefined`);
check(idle, '1. nothing is made before the person asks for it', JSON.stringify(await where(tab)));
const ready = await makePrompt(tab);
check(ready, '1. the press that asks for it makes the prompt, and shows it', await pageText(tab));
// The cause of the bug: the page joins the Lounge behind the first screen,
// which writes memberships. Wait for that, then reload.
const joined = await until(tab, `!!localStorage.getItem('tilo-host-memberships')`, stepLimit(3));
check(joined, '1. the Lounge let the tab in behind the first screen (memberships kept)');
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === '' && frame.offer === ready, '1. reloaded before copy: the start, with the same prompt, at the first frame', JSON.stringify(frame));
check(!await inLounge(tab), '1. reloaded before copy: not in the Lounge');
shot(tab, 'resume-1-before-copy');

// 2. After copy.
await until(tab, `document.getElementById('first-screen')?.dataset.setup === 'ready'`);
const shown = await tab.evaluate(`document.getElementById('first-screen-offer').textContent`);
const prompt = await copy(tab, 'first-screen-agent');
check(prompt && prompt === shown, '2. the click copies the prompt shown', `${prompt} / ${shown}`);
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === 'waiting' && frame.prompt === prompt, '2. reloaded after copy: waiting, same prompt, at the first frame', JSON.stringify(frame));
check(await until(tab, `document.getElementById('first-screen-net')?.textContent === ''`, stepLimit(2)),
  '2. reloaded after copy: confirmed by the host', await pageText(tab));
shot(tab, 'resume-2-after-copy');
await tab.navigate('about:blank');
await until(tab, `location.href === 'about:blank'`, stepLimit(1));
await goBack(tab);
await until(tab, `location.origin === ${JSON.stringify(new URL(site).origin)} && document.readyState !== 'loading'`);
frame = await firstFrame(tab);
check(frame.step === 'waiting' && frame.prompt === prompt, '2. back after leaving: waiting, same prompt', JSON.stringify(frame));
shot(tab, 'resume-2-back');
const second = await tabIn(person, 'second-tab');
await second.navigate(site);
frame = await firstFrame(second);
check(frame.step === 'waiting' && frame.prompt === prompt, '2. a second tab: the same step and prompt, no competing setup', JSON.stringify(frame));
shot(second, 'resume-2-second-tab');
// Back in the second tab: the first follows it back to the start. The first
// tab hears it through storage and draws again, which is two steps of its own
// after the press (the write reaching it, and its screen showing the start);
// a starved machine takes a third, so the wait is that long rather than the
// one a tab presses in.
check(await click(second, 'first-screen-back') && await until(second, `document.documentElement.dataset.firstScreen === ''`, stepLimit(1)),
  '2. back in the second tab: it is back at the start', await pageText(second));
check(await until(tab, `document.documentElement.dataset.firstScreen === ''`, stepLimit(3)), '2. back in the second tab: the first tab is back at the start too',
  JSON.stringify({first: await tab.evaluate(`({screen: document.documentElement.dataset.firstScreen ?? null, kept: localStorage.getItem('tilo-setup')})`).catch(() => null)}));
await close(second);
// Copy again in the first tab (a fresh code), then close it and reopen. It
// offers the prompt again at the start, which is the state the copy is made
// on: a tab still on the step it was at copies that step's code.
const offeredB = await makePrompt(tab);
check(codeOf(offeredB) && codeOf(offeredB) !== codeOf(prompt),
  '2. the first tab offers a fresh setup ticket after Back', JSON.stringify(await where(tab)));
// What the person pastes: the clipboard, or (where this browser will not
// read it back to the harness in a tab that was behind another) the prompt
// the page shows once it says it copied it; case 2 proved those are one.
const readB = await copy(tab, 'first-screen-agent');
const promptB = readB ?? await until(tab, `document.getElementById('first-screen')?.dataset.copied === 'true'
  && document.getElementById('first-screen-prompt').textContent`, stepLimit(1));
report.promptBFrom = readB ? 'clipboard' : 'screen';
check(promptB && promptB === offeredB && codeOf(promptB) !== codeOf(prompt),
  '2. the new copy after Back is exactly the fresh prompt shown');
check(await until(tab, `document.documentElement.dataset.firstScreen === 'waiting'
  && JSON.parse(localStorage.getItem('tilo-setup') ?? 'null')?.code?.code === ${JSON.stringify(codeOf(promptB))}`, stepLimit(1)),
  '2. the first tab kept the ticket it handed the agent before the tab is closed');
await close(tab);

// 3. Agent installing while the tab is closed: it waits for this browser to approve it.
const agent = login(codeOf(promptB), computer('one'));
check(await Promise.race([agent.asked, sleep(stepLimit(3)).then(() => false)]),
  '3. the agent started the login while the tab was closed');
tab = await tabIn(person, 'reopened');
await tab.navigate(site);
frame = await firstFrame(tab);
check(frame.step === 'waiting' && frame.prompt === promptB, '3. reopened while the agent installs: waiting, same prompt', JSON.stringify(frame));
const loggedIn = await until(tab, `(document.getElementById('first-screen-steps')?.innerText ?? '').includes('logged in as you') || document.documentElement.dataset.firstScreen === 'connected'`, stepLimit(3));
mark('case 3: the agent logged in');
check(loggedIn, '3. the reopened tab approved the agent, which logged in', await pageText(tab));
shot(tab, 'resume-3-agent-installing');
// 4. Signed in: reload.
await reload(tab);
frame = await firstFrame(tab);
// The agent goes on without the tab once it logged in, and may be in by the
// time the page loads again: then the setup is over, and the page opens the
// person's own workspace, not the Lounge.
const doneBefore = frame.step === null && (await setup(tab))?.step === 'done';
report.signedInReload = doneBefore ? 'the agent was in already' : frame.step;
check((['waiting', 'connected'].includes(frame.step) && (frame.step === 'connected' || /Tilo is installed/.test(frame.steps ?? '')))
  || (doneBefore && !await inLounge(tab)),
  '4. reloaded once the agent signed in: what it did so far at the first frame, or its workspace once it was in', JSON.stringify(frame));
shot(tab, 'resume-4-agent-signed-in');
const ran = await agent;
mark('case 4: tilo login finished');
check(ran.code === 0, '3. tilo login finished', `${ran.stderr}${ran.stdout}`.slice(-600));
// 5. Entered: the person's own workspace.
const own = await until(tab, OWN_WORKSPACE, stepLimit(3));
check(own, '5. entered: the tab lands in the person\'s own workspace', JSON.stringify(await where(tab)));
shot(tab, 'resume-5-entered');
check((await setup(tab))?.step === 'done', '5. the setup is kept as done');
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === null, '5. reloaded: no first screen', JSON.stringify(frame));
check(await until(tab, `(${OWN_WORKSPACE}) === ${JSON.stringify(own)}`, stepLimit(3)), '5. reloaded: the own workspace again, not the Lounge',
  JSON.stringify(await where(tab)));
// Back to the bare entry the person first opened, from which their own
// workspace was opened.
await goBack(tab);
await until(tab, `document.readyState !== 'loading'`, stepLimit(2));
check(await until(tab, `(${OWN_WORKSPACE}) === ${JSON.stringify(own)}`, stepLimit(3)), '5. back to the bare entry: the own workspace, not the Lounge',
  JSON.stringify(await where(tab)));
shot(tab, 'resume-5-back');
await close(tab);

// ---- 6. Another person: the agent finishes while the tab is closed.
const later = await browser.context();
tab = await tabIn(later, 'closed-mid-way');
await tab.navigate(site);
// Another person's browser: its person is founded too, so the card comes
// once here before the first screen offers to set up.
await keepRecoveryWords(tab, 'setup 6: founded');
await makePrompt(tab);
const promptC = await copy(tab, 'first-screen-agent');
const agentC = login(codeOf(promptC), computer('two'));
// The screen cannot say the agent logged in before the agent did, and on a
// loaded machine that login outlasts the steps of this flow's pacing: past the
// usual wait, the agent's own end is the clock, then one step for the screen
// to draw what the host said. A screen that never says it fails still.
const LOGGED_IN = `(document.getElementById('first-screen-steps')?.innerText ?? '').includes('logged in as you')`;
check(await until(tab, LOGGED_IN, stepLimit(3)) || await agentC.then(() => until(tab, LOGGED_IN, stepLimit(1)), () => null),
  '6. the agent logged in', await pageText(tab));
await close(tab);
const ranC = await agentC;
mark('case 6: tilo login finished');
check(ranC.code === 0 && /Entered/.test(ranC.stdout), '6. the agent entered the workspace while the tab was closed', `${ranC.stdout}`.slice(-400));
tab = await tabIn(later, 'reopened-after-agent');
await tab.navigate(site);
frame = await firstFrame(tab);
check(['waiting', 'connected'].includes(frame.step), '6. reopened: the setup it was at, at the first frame', JSON.stringify(frame));
const said = await until(tab, `document.documentElement.dataset.firstScreen === 'connected' && document.getElementById('first-screen-connected').textContent`, stepLimit(2));
check(said === 'Reviewer is in your workspace', '6. reopened: "Reviewer is in your workspace"', said ?? await pageText(tab));
shot(tab, 'resume-6-agent-done-while-closed');
check(await until(tab, OWN_WORKSPACE, stepLimit(3)),
  '6. reopened: the own workspace opens', JSON.stringify(await where(tab)));
await close(tab);

// ---- 7 and 8. A third person: a code that ran out, then offline.
const third = await browser.context();
tab = await tabIn(third, 'expiry');
await tab.navigate(site);
await keepRecoveryWords(tab, 'setup 7: founded');
await makePrompt(tab);
const promptD = await copy(tab, 'first-screen-agent');
// The code's ten minutes pass while the tab is closed: kept as having run out.
await tab.evaluate(`(() => { const s = JSON.parse(localStorage.getItem('tilo-setup')); s.code.expiresAt = Date.now() - 1000; localStorage.setItem('tilo-setup', JSON.stringify(s)); })()`);
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === 'expired' && frame.expired, '7. a code that ran out: said at the first frame', JSON.stringify(frame));
const fresh = await until(tab, `document.documentElement.dataset.firstScreen === 'expired' && document.getElementById('first-screen-prompt').textContent`, stepLimit(2));
check(fresh && codeOf(fresh) && fresh !== promptD, '7. a fresh prompt shows beside the note', `${fresh}`);
shot(tab, 'resume-7-expired');
const copiedFresh = await copy(tab, 'first-screen-retry');
check(copiedFresh === fresh, '7. copying it copies the fresh prompt shown', `${copiedFresh} / ${fresh}`);
check(await until(tab, `document.documentElement.dataset.firstScreen === 'waiting'`, stepLimit(1)) && (await setup(tab))?.code?.code === codeOf(fresh),
  '7. and follows the fresh code');
await tab.send('Network.enable', {});
await tab.send('Network.emulateNetworkConditions', {offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1});
const offline = await until(tab, `document.getElementById('first-screen-net')?.textContent`, stepLimit(2));
check(/offline/i.test(offline ?? ''), '8. offline: the screen says so', offline ?? await pageText(tab));
shot(tab, 'resume-8-offline');
check(await where(tab).then(at => at.step === 'waiting'), '8. offline: still on its step');
await tab.send('Network.emulateNetworkConditions', {offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1});
check(await until(tab, `document.getElementById('first-screen-net')?.textContent === ''`, stepLimit(2)), '8. online again: carries on, the note gone', await pageText(tab));
shot(tab, 'resume-8-online');
await close(tab);

// ---- 9. A fourth person: the agent logs in the moment the prompt shows.
// The prompt is made the moment the person asks for it, which is as soon as
// the tab is let into the Lounge; the agent asks for the code at once, and is
// waited for.
const quick = await browser.context();
tab = await tabIn(quick, 'login-at-once');
await tab.navigate(site);
await keepRecoveryWords(tab, 'setup 9: founded');
const offerE = await makePrompt(tab);
const agentE = login(codeOf(offerE), computer('four'));
report.admittedBeforeQuickAgent = await tab.evaluate(`!!localStorage.getItem('tilo-host-memberships')`);
console.log(`9. tilo login started ${report.admittedBeforeQuickAgent ? 'after' : 'before'} the Lounge let the tab in`);
await click(tab, 'first-screen-agent');
check(await until(tab, `(document.getElementById('first-screen-steps')?.innerText ?? '').includes('logged in as you') || document.documentElement.dataset.firstScreen === 'connected'`, stepLimit(3)),
  '9. an agent that logs in at once is approved', await pageText(tab));
const ranE = await agentE;
mark('case 9: tilo login finished');
check(ranE.code === 0 && /Entered/.test(ranE.stdout), '9. tilo login run the moment the prompt showed finishes and enters',
  `${ranE.stderr}${ranE.stdout}`.slice(-600));
check(!/another user identity/.test(ranE.stderr), '9. never refused as another user identity', ranE.stderr.slice(-400));
shot(tab, 'resume-9-login-at-once');
await finish();
