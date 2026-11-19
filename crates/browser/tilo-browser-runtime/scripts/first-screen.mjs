// The entry page's first screen (web/first-screen.mjs) in headless Chrome, at a
// phone's width and a laptop's, light and dark: it shows at the first paint
// with no runtime loaded, fits without sideways scrolling, its primary
// button is in view, it makes no code (and so no workspace) until the person
// presses for one, the click after that copies exactly the prompt shown, a
// code the server does not make is said with a way to try again, the agent's
// steps show live as a fake server reports them, and the returning and manual
// flows read. Screenshots of each state go to SHOTS_DIR.
//
// usage: node first-screen.mjs [WEB_DIR]
//   WEB_DIR  the page's sources (default: ../web); no bundle is needed, since
//            the screen is the page's own markup.
// Exits non-zero on any failure.
import {mkdirSync, writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const web = resolve(process.argv[2] ?? new URL('../web', import.meta.url).pathname);
const shots = process.env.SHOTS_DIR;
if (shots) mkdirSync(shots, {recursive: true});
// A host's login code, as the page offers it, whose steps the script sets
// (`globalThis.polled`).
const FAKE_CODE = `({code: 'K7Q-MX2P', expiresAt: Date.now() + 10 * 60 * 1000,
  follow: async () => structuredClone(globalThis.polled ?? {resolved: false, expired: false, steps: []}),
  signIn: () => new Promise(() => {})})`;

const server = await serve(web);
const browser = await launch();
const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? 'ok' : 'FAIL'} ${what}`);
  if (!ok) failures.push(what);
};

for (const [width, height, mobile] of [[390, 844, true], [1280, 800, false]]) {
  for (const scheme of ['light', 'dark']) {
    const name = `${width}-${scheme}`;
    // A fresh profile each time: the screen is for a browser new here.
    const context = await browser.context();
    const tab = await browser.tab({context, firstScreen: true});
    await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 2, mobile});
    await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    await tab.send('Browser.grantPermissions', {origin: server.url, browserContextId: context, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
    await tab.navigate(`${server.url}/`);
    await tab.waitFor(`document.readyState !== 'loading' && getComputedStyle(document.getElementById('first-screen')).display !== 'none'`, {timeout: 20000});
    const layout = await tab.evaluate(`(() => {
      const button = document.getElementById('first-screen-agent').getBoundingClientRect();
      return {sideways: document.documentElement.scrollWidth > innerWidth || document.getElementById('first-screen').scrollWidth > innerWidth,
        inView: button.top >= 0 && button.bottom <= innerHeight, height: button.height, ground: getComputedStyle(document.getElementById('first-screen')).backgroundColor};
    })()`);
    check(!layout.sideways, `${name}: nothing scrolls sideways`);
    check(layout.inView, `${name}: the primary button is in the first view`);
    check(layout.height >= 44, `${name}: the primary button is a comfortable target (${layout.height}px)`);
    check(scheme === 'light' ? layout.ground !== 'rgb(18, 18, 20)' : layout.ground === 'rgb(18, 18, 20)', `${name}: the ground follows the scheme (${layout.ground})`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}.png`, await tab.screenshot());

    // The page offers setup as it would once it has its person. A first visit
    // makes nothing while the person is still deciding: the button offers the
    // prompt, and the press makes it. The press after that copies it, inside
    // the click, so the clipboard takes it.
    await tab.evaluate(`import('./first-screen.mjs').then(({firstScreen}) => {
      firstScreen.offerSetup(async () => ${FAKE_CODE}, null, {ahead: false});
      firstScreen.offerAgentSignIn(async () => ${FAKE_CODE});
    })`);
    const idle = await tab.evaluate(`({asked: document.getElementById('first-screen').dataset.setup ?? null,
      well: getComputedStyle(document.querySelector('.fr-offer')).display !== 'none',
      label: document.getElementById('first-screen-agent').innerText.trim()})`);
    check(idle.asked === null, `${name}: nothing is made before the person asks (${idle.asked})`);
    check(!idle.well, `${name}: and no prompt box stands empty for it`);
    check(idle.label === 'Set up with your agent', `${name}: the button offers to set up (${idle.label})`);
    const click = async id => {
      const at = await tab.evaluate(`(() => { const r = document.getElementById('${id}').getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2}; })()`);
      for (const type of ['mousePressed', 'mouseReleased']) {
        await tab.send('Input.dispatchMouseEvent', {type, x: at.x, y: at.y, button: 'left', clickCount: 1});
      }
    };
    const passkey = await tab.evaluate(`(() => { const b = getComputedStyle(document.getElementById('first-screen-passkey')); return {disabled: document.getElementById('first-screen-passkey').disabled, color: b.color}; })()`);
    check(!passkey.disabled, `${name}: the passkey button reads as one to press before the runtime is in`);

    // Set up manually: the MCP server and the commands, with a code the press
    // makes.
    await click('first-screen-manual-toggle');
    await tab.waitFor(`document.getElementById('first-screen-commands').textContent.includes('tilo login K7Q-MX2P')`, {timeout: 10000});
    check(await tab.evaluate(`document.getElementById('first-screen-mcp').textContent === 'https://mcp.tilo.sh'`), `${name}: manual shows the MCP server`);
    check(!(await tab.evaluate(`document.getElementById('first-screen').scrollWidth > innerWidth`)), `${name}: manual fits without scrolling sideways`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-manual.png`, await tab.screenshot());
    await click('first-screen-manual-toggle');

    const shown = await tab.evaluate(`document.getElementById('first-screen-offer').textContent`);
    check(shown.includes('tilo login K7Q-MX2P'), `${name}: the press made the prompt, and it shows`);
    check(!(await tab.evaluate(`document.getElementById('first-screen').scrollWidth > innerWidth`)), `${name}: the shown prompt wraps rather than scrolling sideways`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-prompt.png`, await tab.screenshot());

    const BOX = `(() => { const r = document.querySelector('.fr-offer').getBoundingClientRect();
      return {x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height),
        caption: getComputedStyle(document.querySelector('.fr-caption')).display === 'none' ? null : document.querySelector('.fr-caption').textContent}; })()`;
    const before = await tab.evaluate(BOX);
    await click('first-screen-agent');
    await tab.waitFor(`document.documentElement.dataset.firstScreen === 'waiting'`, {timeout: 10000});
    const after = await tab.evaluate(BOX);
    check(before.x === after.x && before.width === after.width,
      `${name}: the prompt box stays where it was, as wide (${JSON.stringify(before)} → ${JSON.stringify(after)})`);
    check(before.height === after.height, `${name}: the prompt box keeps its height (${before.height} → ${after.height})`);
    check(Math.abs(before.y - after.y) <= 1, `${name}: the prompt box does not jump (${before.y} → ${after.y})`);
    check(before.caption === 'The code in it works once, for 10 minutes, in this tab only.', `${name}: the caption says how long the code works`);
    check(after.caption === null, `${name}: once copied, the countdown alone says how long it works`);
    const copied = await tab.evaluate(`Promise.race([navigator.clipboard.readText().catch(() => null), new Promise(r => setTimeout(() => r(null), 3000))])`);
    check(copied === null || copied === shown, `${name}: the click copied exactly the prompt shown${copied === null ? ' (clipboard not readable here)' : ''}`);
    check(await tab.evaluate(`document.getElementById('first-screen-prompt').textContent`) === shown, `${name}: the prompt stays on the screen`);
    if (copied !== null) {
      check(await tab.evaluate(`getComputedStyle(document.querySelector('.fr-copied')).display !== 'none'`), `${name}: the screen says it copied`);
    }
    const waiting = await tab.evaluate(`({state: document.documentElement.dataset.firstScreen, sideways: document.getElementById('first-screen').scrollWidth > innerWidth,
      left: document.getElementById('first-screen-left').textContent, step: document.getElementById('first-screen-steps').textContent})`);
    check(waiting.state === 'waiting', `${name}: the screen waits for the agent`);
    check(!waiting.sideways, `${name}: the prompt wraps rather than scrolling sideways`);
    check(waiting.left === 'Works once, for 10 more minutes.', `${name}: it says how long the prompt works (${waiting.left})`);
    check(waiting.step === 'Waiting for your agent', `${name}: nothing is claimed before the agent used the code (${waiting.step})`);
    check(!(await tab.evaluate(`document.querySelector('#first-screen-steps li')?.className === 'now'`)), `${name}: no spinner while nothing happens`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-copied.png`, await tab.screenshot());
    await tab.evaluate(`globalThis.polled = {resolved: true, expired: false, agent: 'Claude Code', steps: ['resolved', 'logged-in']}`);
    await tab.waitFor(`document.getElementById('first-screen-steps').textContent.includes('Claude Code logged in as you')`, {timeout: 5000});
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-waiting.png`, await tab.screenshot());

    // The agent fails partway: the step it stopped at, not under way, and its reason, with a new prompt to try again.
    await tab.evaluate(`globalThis.polled = {resolved: true, expired: false, agent: 'Claude Code', steps: ['resolved', 'logged-in', 'tools-added', 'restart', 'failed'],
      detail: 'Logged in, but this computer could not open the workspace yet: the host could not be reached. Your agent tries again when it next uses Tilo.'}`);
    await tab.waitFor(`document.getElementById('first-screen').dataset.agentFailed === 'true'`, {timeout: 5000});
    const failedShown = await tab.evaluate(`({step: document.querySelector('#first-screen-steps li:last-child')?.className + ': ' + document.querySelector('#first-screen-steps li:last-child')?.textContent,
      problem: document.getElementById('first-screen-problem').textContent, retry: getComputedStyle(document.getElementById('first-screen-retry')).display !== 'none',
      spinning: !!document.querySelector('#first-screen-steps li.now')})`);
    check(failedShown.step === 'failed: Claude Code could not enter your workspace', `${name}: a failure shows the step the agent stopped at (${failedShown.step})`);
    check(failedShown.problem.startsWith('Logged in, but entering'), `${name}: and the agent's reason (${failedShown.problem})`);
    check(failedShown.retry && !failedShown.spinning, `${name}: with a new prompt to try again, and nothing claimed under way`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-agent-failed.png`, await tab.screenshot());

    await tab.evaluate(`globalThis.polled = {resolved: true, expired: false, agent: 'Claude Code', steps: ['resolved', 'logged-in', 'tools-added', 'entered']}`);
    await tab.waitFor(`document.documentElement.dataset.firstScreen === 'connected'`, {timeout: 5000});
    await sleep(500);
    check(await tab.evaluate(`document.getElementById('first-screen-connected').textContent === 'Claude Code is in your workspace'`),
      `${name}: the screen says the agent is in the workspace`);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-connected.png`, await tab.screenshot());
    // It stays, saying it opens the person's workspace, until the page can
    // (no runtime here: the page is let in nowhere), then leaves.
    await tab.waitFor(`JSON.parse(localStorage.getItem('tilo-setup')).step === 'opening'`, {timeout: 5000});
    check(await tab.evaluate(`!!document.getElementById('first-screen') && document.getElementById('first-screen-done-lead').textContent === 'Opening your workspace…'`),
      `${name}: the screen says it opens the workspace until it can`);
    await tab.evaluate(`import('./first-screen.mjs').then(({firstScreen}) => firstScreen.whenDone(() => {}))`);
    await tab.waitFor(`!document.getElementById('first-screen')`, {timeout: 5000});
    check(await tab.evaluate(`JSON.parse(localStorage.getItem('tilo-setup')).step === 'done'`), `${name}: the screen is not shown again`);
    await tab.close();

    // Returning: sign in with your agent, on a fresh profile.
    const returning = await browser.context();
    const again = await browser.tab({context: returning, firstScreen: true});
    await again.send('Browser.grantPermissions', {origin: server.url, browserContextId: returning, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
    await again.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 2, mobile});
    await again.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    await again.navigate(`${server.url}/`);
    await again.waitFor(`getComputedStyle(document.getElementById('first-screen')).display !== 'none'`, {timeout: 20000});
    await again.evaluate(`import('./first-screen.mjs').then(({firstScreen}) => { firstScreen.offerAgentSignIn(async () => ${FAKE_CODE}); })`);
    await again.waitFor(`document.getElementById('first-screen').dataset.signIn === 'ready'`, {timeout: 10000});
    const button = await again.evaluate(`(() => { const r = document.getElementById('first-screen-agent-sign-in').getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2}; })()`);
    for (const type of ['mousePressed', 'mouseReleased']) {
      await again.send('Input.dispatchMouseEvent', {type, x: button.x, y: button.y, button: 'left', clickCount: 1});
    }
    await again.waitFor(`document.getElementById('first-screen-prompt').textContent.match(/(?:tilo|sh -s) login /)`, {timeout: 10000});
    await again.evaluate(`globalThis.polled = {resolved: true, expired: false, steps: ['resolved'], claimed: {user: '${'ab'.repeat(32)}', invite: '${'cd'.repeat(16)}', by: 'Claude Code on build-box'}}`);
    await again.waitFor(`document.getElementById('first-screen-steps').textContent.includes('vouched for you')`, {timeout: 5000});
    check(await again.evaluate(`document.getElementById('first-screen-prompt').textContent.startsWith('Sign me in to Tilo: run tilo login K7Q-MX2P')`),
      `${name}: the returning prompt is the short sign-in one`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-returning.png`, await again.screenshot());
    await again.close();

    // A host that does not answer: the screen says so, with a way to try
    // again, and the button copies nothing.
    const silent = await browser.context();
    const stuck = await browser.tab({context: silent, firstScreen: true});
    await stuck.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 2, mobile});
    await stuck.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    await stuck.navigate(`${server.url}/`);
    await stuck.waitFor(`getComputedStyle(document.getElementById('first-screen')).display !== 'none'`, {timeout: 20000});
    await stuck.evaluate(`import('./first-screen.mjs').then(({firstScreen}) => {
      firstScreen.offerSetup(async () => { throw Object.assign(new TypeError('Failed to fetch'), {code: 'unreachable'}); }, null, {ahead: false});
    })`);
    await stuck.evaluate(`document.getElementById('first-screen-agent').click()`);
    await stuck.waitFor(`document.getElementById('first-screen').dataset.setup === 'failed'`, {timeout: 10000});
    const said = await stuck.evaluate(`({note: document.getElementById('first-screen-offer-note').textContent,
      retry: !document.getElementById('first-screen-offer-retry').hidden})`);
    check(/^Couldn’t reach /.test(said.note), `${name}: a host that does not answer is said (${said.note})`);
    check(said.retry, `${name}: with a way to try again`);
    check(!(await stuck.evaluate(`document.body.textContent.includes('Preparing')`)), `${name}: no placeholder stands in for a reason`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-unreachable.png`, await stuck.screenshot());
    await stuck.close();
  }
}

await browser.close();
await server.close();
if (failures.length) {
  console.error(`${failures.length} failed`);
  process.exit(1);
}
