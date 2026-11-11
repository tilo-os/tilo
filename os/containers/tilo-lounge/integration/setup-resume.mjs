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
//   1. before copy: a reload after the Lounge let the tab in shows the start,
//      with the same prompt;
//   2. after copy: a reload, back and forward, a second tab, and a closed and
//      reopened tab show "Paste it into your agent" with the same prompt;
//   3. agent installing: the agent runs `tilo login` while the tab is closed,
//      and reopening it approves the agent and follows it;
//   4. agent signed in: a reload once it logged in shows what it did so far;
//   5. entered: the tab lands in the person's own workspace, and a reload,
//      back and forward land there again;
//   6. agent finishes while the tab is closed: reopening says "Claude Code is
//      in your workspace" and opens it;
//   7. code expiry: a code that ran out while the tab was closed shows a fresh
//      one with a note, and copying it follows the fresh one;
//   8. offline, then online: the screen says it is offline, then carries on;
//   9. an agent that runs `tilo login` the moment the prompt shows, before the
//      Lounge let the tab in, is waited for, never refused, and gets in.
//
// usage: node setup-resume.mjs SITE_URL RELAY_ORIGIN TILO_BIN SITE_CA OUT_DIR
//
// Saves a capture of each resumed state and a JSON report to OUT_DIR; exits
// non-zero when any case lands on the wrong screen.
import {execFile} from 'node:child_process';
import {chmodSync, mkdirSync, writeFileSync} from 'node:fs';
import {connect} from 'node:tls';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, tiloBin, siteCa, out] = process.argv.slice(2);
if (!site || !relay || !tiloBin || !siteCa || !out) {
  console.error('usage: node setup-resume.mjs SITE_URL RELAY_ORIGIN TILO_BIN SITE_CA OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const report = {site, relay, at: new Date().toISOString(), cases: []};
const failures = [];
const open = [];
const check = (ok, what, detail = '') => {
  report.cases.push({ok: Boolean(ok), what, ...(detail ? {detail} : {})});
  console.log(`${ok ? 'ok' : 'FAIL'} ${what}${!ok && detail ? `: ${detail}` : ''}`);
  if (!ok) failures.push(what);
  return Boolean(ok);
};

const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
const finish = async () => {
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
// after the page's body is parsed past the screen (the page can paint no
// sooner), before any module of the page has run.
const FIRST_FRAME = `(() => {
  const look = () => {
    if (!document.getElementById('surface')) { requestAnimationFrame(look); return; }
    const text = id => document.getElementById(id)?.textContent ?? null;
    const expired = document.querySelector('.fr-expired');
    globalThis.__firstFrame = {step: document.documentElement.dataset.firstRun ?? null, offer: text('first-run-offer'),
      prompt: text('first-run-prompt'), steps: text('first-run-steps'), heading: text('first-run-connected'),
      net: text('first-run-net'), expired: !!expired && getComputedStyle(expired).display !== 'none',
      modules: !!globalThis.__tiloFirstRunReady};
  };
  requestAnimationFrame(look);
})();`;

// A tab of one browser profile (`context`), its first screen not marked seen.
const tabIn = async (context, name) => {
  const tab = await browser.tab({context, firstRun: true});
  open.push({name, tab});
  await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: FIRST_FRAME});
  // Each tab counts as focused, as the one a person works in: the clipboard
  // answers only a focused page, and several tabs share this browser.
  await tab.send('Emulation.setFocusEmulationEnabled', {enabled: true}).catch(() => {});
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
  await tab.send('Browser.grantPermissions', {origin: new URL(site).origin, browserContextId: context,
    permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
  return tab;
};
const shot = async (tab, name) => {
  await sleep(400);
  const png = await tab.screenshot().catch(() => null);
  if (png) writeFileSync(join(out, `${name}.png`), png);
  report.captures = [...(report.captures ?? []), `${name}.png`];
  // Where a check's outputs cannot be fetched, the log carries the capture
  // too, small, when asked (SETUP_RESUME_LOG_SHOTS=1).
  if (process.env.SETUP_RESUME_LOG_SHOTS === '1') {
    const jpeg = await tab.send('Page.captureScreenshot', {format: 'jpeg', quality: 55}).catch(() => null);
    // In pieces: a log line is cut after 16 KiB.
    const data = jpeg?.result?.data ?? '';
    const pieces = Math.ceil(data.length / 12_000);
    for (let piece = 0; piece < pieces; piece += 1) {
      console.log(`SHOT ${name} ${piece + 1}/${pieces} ${data.slice(piece * 12_000, (piece + 1) * 12_000)}`);
    }
  }
};
const until = async (tab, expression, timeout = 90_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await tab.evaluate(expression).catch(() => null);
    if (value) return value;
    if (Date.now() > deadline) return null;
    await sleep(200);
  }
};
const firstFrame = tab => until(tab, 'globalThis.__firstFrame && JSON.stringify(globalThis.__firstFrame)', 30_000).then(text => JSON.parse(text ?? '{}'));
const setup = tab => tab.evaluate(`localStorage.getItem('tilo-setup')`).then(text => JSON.parse(text ?? 'null')).catch(() => null);
const pageText = tab => tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 400)`).catch(() => '');
// Where the tab is: the first screen's step, or the workspace it is titled after.
const where = tab => tab.evaluate(`JSON.stringify({step: document.documentElement.dataset.firstRun ?? null,
  screen: document.documentElement.dataset.firstRun !== undefined, title: document.title, entry: document.documentElement.dataset.entry ?? null})`)
  .then(JSON.parse).catch(() => ({}));
const inLounge = async tab => {
  const at = await where(tab);
  return !at.screen && /lounge/i.test(at.title);
};
const forget = tab => tab.evaluate('delete globalThis.__firstFrame').catch(() => {});
const reload = async tab => { await forget(tab); await tab.reload(); await sleep(50); };
// Back one entry in the tab's history, as the browser's Back button goes.
const goBack = async tab => { await forget(tab); await tab.evaluate('history.back()').catch(() => {}); await sleep(300); };
// A person's click: on the tab in front, as only that one may use the clipboard.
const click = async (tab, id) => {
  await tab.send('Page.bringToFront').catch(() => {});
  const box = JSON.parse(await until(tab, `(() => { const e = document.getElementById(${JSON.stringify(id)});
    if (!e) return ''; const r = e.getBoundingClientRect(); if (!r.width) return '';
    return JSON.stringify({x: r.x + r.width / 2, y: r.y + r.height / 2}); })()`) ?? 'null');
  if (!box) return false;
  for (const type of ['mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x: box.x, y: box.y, button: 'left', clickCount: 1});
  }
  return true;
};
const clipboard = tab => until(tab, `navigator.clipboard.readText().then(text => text.includes('tilo login') ? text : '', () => '')`, 10_000);
const codeOf = prompt => prompt?.match(/tilo login ([2-9A-HJKMNP-TV-Z]{3}-[2-9A-HJKMNP-TV-Z]{4})/)?.[1] ?? null;
const steps = tab => tab.evaluate(`document.getElementById('first-run-steps')?.innerText ?? ''`).catch(() => '');

// The agent's computer, as agent-setup.mjs makes it: its own home, a
// `claude` that records how it was asked, Claude Code's marker, and the
// test certificates trusted only through TILO_TEST_CA_FILE.
const relayPem = await new Promise((resolve, reject) => {
  const {hostname, port} = new URL(relay);
  const socket = connect({host: hostname, port: Number(port || 443), rejectUnauthorized: false}, () => {
    const der = socket.getPeerCertificate(false).raw;
    socket.end();
    resolve(`-----BEGIN CERTIFICATE-----\n${der.toString('base64').match(/.{1,64}/g).join('\n')}\n-----END CERTIFICATE-----\n`);
  });
  socket.on('error', reject);
});
const computer = name => {
  const home = join(out, `computer-${name}`);
  mkdirSync(join(home, 'bin'), {recursive: true});
  // As agent-setup.mjs's: `claude mcp add` writes the server into the user
  // configuration (`~/.claude.json`), where `tilo login` confirms it.
  writeFileSync(join(home, 'bin', 'claude'), `#!/bin/sh
[ "$1 $2" = "mcp add" ] || exit 0
shift 2
while [ $# -gt 0 ]; do
  case "$1" in
    --scope|-s) shift 2 ;;
    --*) shift ;;
    *) break ;;
  esac
done
name=$1; shift
[ "$1" = "--" ] && shift
command=$1; shift
args=
for arg in "$@"; do args="$args\\"$arg\\","; done
printf '{"mcpServers":{"%s":{"type":"stdio","command":"%s","args":[%s],"env":{}}}}\\n' \\
  "$name" "$command" "\${args%,}" > "$HOME/.claude.json"
`);
  chmodSync(join(home, 'bin', 'claude'), 0o755);
  writeFileSync(join(home, 'relay.pem'), relayPem);
  return {PATH: `${join(home, 'bin')}:${process.env.PATH}`, HOME: home, XDG_STATE_HOME: join(home, 'state'),
    XDG_CONFIG_HOME: join(home, 'config'), CLAUDECODE: '1', TILO_TEST_CA_FILE: `${siteCa}:${join(home, 'relay.pem')}`};
};
const login = (code, env) => new Promise(resolve => {
  execFile(tiloBin, ['login', code, '--site', site], {env, timeout: 600_000, maxBuffer: 1 << 20},
    (error, stdout, stderr) => resolve({code: error ? (error.code ?? 1) : 0, stdout, stderr}));
});

// ---- One person, through every step.
const person = await browser.context();
let tab = await tabIn(person, 'setup');
await tab.navigate(site);
let frame = await firstFrame(tab);
check(frame.step === '', '1. a browser new here opens on the first screen at its first frame', JSON.stringify(frame));
const ready = await until(tab, `document.getElementById('first-run')?.dataset.setup === 'ready' && document.getElementById('first-run-offer').textContent`);
check(ready, '1. the setup prompt shows before the click', await pageText(tab));
// The cause of the bug: the page joins the Lounge behind the first screen,
// which writes memberships. Wait for that, then reload.
const joined = await until(tab, `!!localStorage.getItem('tilo-host-memberships')`, 120_000);
check(joined, '1. the Lounge let the tab in behind the first screen (memberships kept)');
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === '' && frame.offer === ready, '1. reloaded before copy: the start, with the same prompt, at the first frame', JSON.stringify(frame));
check(!await inLounge(tab), '1. reloaded before copy: not in the Lounge');
await shot(tab, 'resume-1-before-copy');

// 2. After copy.
await until(tab, `document.getElementById('first-run')?.dataset.setup === 'ready'`);
const shown = await tab.evaluate(`document.getElementById('first-run-offer').textContent`);
await click(tab, 'first-run-agent');
const prompt = await clipboard(tab);
check(prompt && prompt === shown, '2. the click copies the prompt shown', `${prompt} / ${shown}`);
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === 'waiting' && frame.prompt === prompt, '2. reloaded after copy: waiting, same prompt, at the first frame', JSON.stringify(frame));
check(await until(tab, `document.getElementById('first-run-net')?.textContent === ''`, 30_000),
  '2. reloaded after copy: confirmed by the host', await pageText(tab));
await shot(tab, 'resume-2-after-copy');
await tab.navigate('about:blank');
await until(tab, `location.href === 'about:blank'`, 10_000);
await goBack(tab);
await until(tab, `location.origin === ${JSON.stringify(new URL(site).origin)} && document.readyState !== 'loading'`);
frame = await firstFrame(tab);
check(frame.step === 'waiting' && frame.prompt === prompt, '2. back after leaving: waiting, same prompt', JSON.stringify(frame));
await shot(tab, 'resume-2-back');
const second = await tabIn(person, 'second-tab');
await second.navigate(site);
frame = await firstFrame(second);
check(frame.step === 'waiting' && frame.prompt === prompt, '2. a second tab: the same step and prompt, no competing setup', JSON.stringify(frame));
await shot(second, 'resume-2-second-tab');
// Back in the second tab: the first follows it back to the start.
await click(second, 'first-run-back');
check(await until(tab, `document.documentElement.dataset.firstRun === ''`, 10_000), '2. back in the second tab: the first tab is back at the start too');
await second.close();
// Copy again in the first tab (a fresh code), then close it and reopen.
await until(tab, `document.getElementById('first-run')?.dataset.setup === 'ready'`);
await click(tab, 'first-run-agent');
// What the person pastes: the clipboard, or (where this browser will not
// read it back to the harness in a tab that was behind another) the prompt
// the page shows once it says it copied it; case 2 proved those are one.
const promptB = await clipboard(tab) ?? await until(tab, `document.getElementById('first-run')?.dataset.copied === 'true'
  && document.getElementById('first-run-prompt').textContent`, 10_000);
report.promptBFrom = (await clipboard(tab)) === promptB ? 'clipboard' : 'screen';
check(codeOf(promptB) && promptB !== prompt, '2. a new copy after Back is a fresh code');
await tab.close();

// 3. Agent installing while the tab is closed: it waits for this browser to approve it.
const agent = login(codeOf(promptB), computer('one'));
await sleep(3_000);
tab = await tabIn(person, 'reopened');
await tab.navigate(site);
frame = await firstFrame(tab);
check(frame.step === 'waiting' && frame.prompt === promptB, '3. reopened while the agent installs: waiting, same prompt', JSON.stringify(frame));
const loggedIn = await until(tab, `(document.getElementById('first-run-steps')?.innerText ?? '').includes('logged in as you') || document.documentElement.dataset.firstRun === 'connected'`, 120_000);
check(loggedIn, '3. the reopened tab approved the agent, which logged in', await pageText(tab));
await shot(tab, 'resume-3-agent-installing');
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
await shot(tab, 'resume-4-agent-signed-in');
const ran = await agent;
check(ran.code === 0, '3. tilo login finished', `${ran.stderr}${ran.stdout}`.slice(-600));
// 5. Entered: the person's own workspace.
const own = await until(tab, `document.documentElement.dataset.firstRun === undefined && /^Tilo · /.test(document.title) && !/lounge/i.test(document.title) && document.title`, 120_000);
check(own, '5. entered: the tab lands in the person\'s own workspace', JSON.stringify(await where(tab)));
await shot(tab, 'resume-5-entered');
check((await setup(tab))?.step === 'done', '5. the setup is kept as done');
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === null, '5. reloaded: no first screen', JSON.stringify(frame));
check(await until(tab, `/^Tilo · /.test(document.title) && document.title === ${JSON.stringify(own)}`, 120_000), '5. reloaded: the own workspace again, not the Lounge',
  JSON.stringify(await where(tab)));
// Back to the bare entry the person first opened, from which their own
// workspace was opened.
await goBack(tab);
await until(tab, `document.readyState !== 'loading'`, 30_000);
check(await until(tab, `document.title === ${JSON.stringify(own)}`, 120_000), '5. back to the bare entry: the own workspace, not the Lounge',
  JSON.stringify(await where(tab)));
await shot(tab, 'resume-5-back');
await tab.close();

// ---- 6. Another person: the agent finishes while the tab is closed.
const later = await browser.context();
tab = await tabIn(later, 'closed-mid-way');
await tab.navigate(site);
await until(tab, `document.getElementById('first-run')?.dataset.setup === 'ready'`);
await click(tab, 'first-run-agent');
const promptC = await clipboard(tab);
const agentC = login(codeOf(promptC), computer('two'));
check(await until(tab, `(document.getElementById('first-run-steps')?.innerText ?? '').includes('logged in as you')`, 120_000),
  '6. the agent logged in', await pageText(tab));
await tab.close();
const ranC = await agentC;
check(ranC.code === 0 && /Entered/.test(ranC.stdout), '6. the agent entered the workspace while the tab was closed', `${ranC.stdout}`.slice(-400));
tab = await tabIn(later, 'reopened-after-agent');
await tab.navigate(site);
frame = await firstFrame(tab);
check(['waiting', 'connected'].includes(frame.step), '6. reopened: the setup it was at, at the first frame', JSON.stringify(frame));
const said = await until(tab, `document.documentElement.dataset.firstRun === 'connected' && document.getElementById('first-run-connected').textContent`, 60_000);
check(said === 'Claude Code is in your workspace', '6. reopened: "Claude Code is in your workspace"', said ?? await pageText(tab));
await shot(tab, 'resume-6-agent-done-while-closed');
check(await until(tab, `document.documentElement.dataset.firstRun === undefined && /^Tilo · /.test(document.title) && !/lounge/i.test(document.title)`, 120_000),
  '6. reopened: the own workspace opens', JSON.stringify(await where(tab)));
await tab.close();

// ---- 7 and 8. A third person: a code that ran out, then offline.
const third = await browser.context();
tab = await tabIn(third, 'expiry');
await tab.navigate(site);
await until(tab, `document.getElementById('first-run')?.dataset.setup === 'ready'`);
await click(tab, 'first-run-agent');
const promptD = await clipboard(tab);
// The code's ten minutes pass while the tab is closed: kept as having run out.
await tab.evaluate(`(() => { const s = JSON.parse(localStorage.getItem('tilo-setup')); s.code.expiresAt = Date.now() - 1000; localStorage.setItem('tilo-setup', JSON.stringify(s)); })()`);
await reload(tab);
frame = await firstFrame(tab);
check(frame.step === 'expired' && frame.expired, '7. a code that ran out: said at the first frame', JSON.stringify(frame));
const fresh = await until(tab, `document.documentElement.dataset.firstRun === 'expired' && document.getElementById('first-run-prompt').textContent`, 30_000);
check(fresh && codeOf(fresh) && fresh !== promptD, '7. a fresh prompt shows beside the note', `${fresh}`);
await shot(tab, 'resume-7-expired');
await click(tab, 'first-run-retry');
const copiedFresh = await clipboard(tab);
check(copiedFresh === fresh, '7. copying it copies the fresh prompt shown', `${copiedFresh} / ${fresh}`);
check(await until(tab, `document.documentElement.dataset.firstRun === 'waiting'`, 10_000) && (await setup(tab))?.code?.code === codeOf(fresh),
  '7. and follows the fresh code');
await tab.send('Network.enable', {});
await tab.send('Network.emulateNetworkConditions', {offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1});
const offline = await until(tab, `document.getElementById('first-run-net')?.textContent`, 30_000);
check(/offline/i.test(offline ?? ''), '8. offline: the screen says so', offline ?? await pageText(tab));
await shot(tab, 'resume-8-offline');
check(await where(tab).then(at => at.step === 'waiting'), '8. offline: still on its step');
await tab.send('Network.emulateNetworkConditions', {offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1});
check(await until(tab, `document.getElementById('first-run-net')?.textContent === ''`, 30_000), '8. online again: carries on, the note gone', await pageText(tab));
await shot(tab, 'resume-8-online');
await tab.close();

// ---- 9. A fourth person: the agent logs in the moment the prompt shows.
// The tab is let into the Lounge behind the first screen, after its prompt is
// ready; an agent this quick asks before that, and is waited for.
const quick = await browser.context();
tab = await tabIn(quick, 'login-at-once');
await tab.navigate(site);
const offerE = await until(tab, `document.getElementById('first-run')?.dataset.setup === 'ready' && document.getElementById('first-run-offer').textContent`);
const agentE = login(codeOf(offerE), computer('four'));
report.admittedBeforeQuickAgent = await tab.evaluate(`!!localStorage.getItem('tilo-host-memberships')`);
console.log(`9. tilo login started ${report.admittedBeforeQuickAgent ? 'after' : 'before'} the Lounge let the tab in`);
await click(tab, 'first-run-agent');
check(await until(tab, `(document.getElementById('first-run-steps')?.innerText ?? '').includes('logged in as you') || document.documentElement.dataset.firstRun === 'connected'`, 120_000),
  '9. an agent that logs in at once is approved', await pageText(tab));
const ranE = await agentE;
check(ranE.code === 0 && /Entered/.test(ranE.stdout), '9. tilo login run the moment the prompt showed finishes and enters',
  `${ranE.stderr}${ranE.stdout}`.slice(-600));
check(!/another user identity/.test(ranE.stderr), '9. never refused as another user identity', ranE.stderr.slice(-400));
await shot(tab, 'resume-9-login-at-once');
await finish();
