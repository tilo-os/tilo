// The entry page's first screen (web/first-run.mjs) in headless Chrome, at a
// phone's width and a laptop's, light and dark: it shows at the first paint
// with no runtime loaded, fits without sideways scrolling, its primary
// button is in view and copies inside the click, the agent's steps show
// live as a fake host reports them, and the returning and manual flows
// read. Screenshots of each state go to SHOTS_DIR.
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
    const tab = await browser.tab({context, firstRun: true});
    await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 2, mobile});
    await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    await tab.send('Browser.grantPermissions', {origin: server.url, browserContextId: context, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
    await tab.navigate(`${server.url}/`);
    await tab.waitFor(`document.readyState !== 'loading' && getComputedStyle(document.getElementById('first-run')).display !== 'none'`, {timeout: 20000});
    const layout = await tab.evaluate(`(() => {
      const button = document.getElementById('first-run-agent').getBoundingClientRect();
      return {sideways: document.documentElement.scrollWidth > innerWidth || document.getElementById('first-run').scrollWidth > innerWidth,
        inView: button.top >= 0 && button.bottom <= innerHeight, height: button.height, ground: getComputedStyle(document.getElementById('first-run')).backgroundColor};
    })()`);
    check(!layout.sideways, `${name}: nothing scrolls sideways`);
    check(layout.inView, `${name}: the primary button is in the first view`);
    check(layout.height >= 44, `${name}: the primary button is a comfortable target (${layout.height}px)`);
    check(scheme === 'light' ? layout.ground !== 'rgb(18, 18, 20)' : layout.ground === 'rgb(18, 18, 20)', `${name}: the ground follows the scheme (${layout.ground})`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}.png`, await tab.screenshot());

    // The page offers setup as it would once it is in; the click is a real
    // one, so the clipboard takes the prompt inside it.
    await tab.evaluate(`import('./first-run.mjs').then(({firstRun}) => {
      firstRun.offerSetup(async () => ${FAKE_CODE});
      firstRun.offerAgentSignIn(async () => ${FAKE_CODE});
    })`);
    const click = async id => {
      const at = await tab.evaluate(`(() => { const r = document.getElementById('${id}').getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2}; })()`);
      for (const type of ['mousePressed', 'mouseReleased']) {
        await tab.send('Input.dispatchMouseEvent', {type, x: at.x, y: at.y, button: 'left', clickCount: 1});
      }
    };
    const passkey = await tab.evaluate(`(() => { const b = getComputedStyle(document.getElementById('first-run-passkey')); return {disabled: document.getElementById('first-run-passkey').disabled, color: b.color}; })()`);
    check(!passkey.disabled, `${name}: the passkey button reads as one to press before the runtime is in`);

    // Set up manually: the MCP server and the commands, with a code.
    await click('first-run-manual-toggle');
    await tab.waitFor(`document.getElementById('first-run-commands').textContent.includes('tilo login K7Q-MX2P')`, {timeout: 10000});
    check(await tab.evaluate(`document.getElementById('first-run-mcp').textContent === 'https://mcp.tilo.sh'`), `${name}: manual shows the MCP server`);
    check(!(await tab.evaluate(`document.getElementById('first-run').scrollWidth > innerWidth`)), `${name}: manual fits without sideways scrolling`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-manual.png`, await tab.screenshot());
    await click('first-run-manual-toggle');

    await click('first-run-agent');
    await tab.waitFor(`document.getElementById('first-run-prompt').textContent.includes('tilo login')`, {timeout: 10000});
    const copied = await tab.evaluate(`Promise.race([navigator.clipboard.readText().catch(() => null), new Promise(r => setTimeout(() => r(null), 3000))])`);
    check(copied === null || copied.includes('tilo login K7Q-MX2P'), `${name}: the click copied the prompt with the code${copied === null ? ' (clipboard not readable here)' : ''}`);
    const waiting = await tab.evaluate(`({state: document.documentElement.dataset.firstRun, sideways: document.getElementById('first-run').scrollWidth > innerWidth,
      left: document.getElementById('first-run-left').textContent, step: document.getElementById('first-run-steps').textContent})`);
    check(waiting.state === 'waiting', `${name}: the screen waits for the agent`);
    check(!waiting.sideways, `${name}: the prompt wraps rather than scrolling sideways`);
    check(/Works once, for \d+:\d\d more/.test(waiting.left), `${name}: it says how long the prompt works (${waiting.left})`);
    check(waiting.step.includes('installing Tilo'), `${name}: the first step shows at once`);
    await tab.evaluate(`globalThis.polled = {resolved: true, expired: false, agent: 'Claude Code', steps: ['resolved', 'logged-in']}`);
    await tab.waitFor(`document.getElementById('first-run-steps').textContent.includes('Claude Code logged in as you')`, {timeout: 5000});
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-waiting.png`, await tab.screenshot());

    await tab.evaluate(`globalThis.polled = {resolved: true, expired: false, agent: 'Claude Code', steps: ['resolved', 'logged-in', 'tools-added', 'entered']}`);
    await tab.waitFor(`document.documentElement.dataset.firstRun === 'connected'`, {timeout: 5000});
    await sleep(500);
    check(await tab.evaluate(`document.getElementById('first-run-connected').textContent === 'Claude Code is in your workspace'`),
      `${name}: the screen says the agent is in the workspace`);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-connected.png`, await tab.screenshot());
    await tab.waitFor(`!document.getElementById('first-run')`, {timeout: 5000});
    check(await tab.evaluate(`localStorage.getItem('tilo-first-run') === '1'`), `${name}: the screen is not shown again`);
    await tab.close();

    // Returning: sign in with your agent, on a fresh profile.
    const returning = await browser.context();
    const again = await browser.tab({context: returning, firstRun: true});
    await again.send('Browser.grantPermissions', {origin: server.url, browserContextId: returning, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
    await again.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 2, mobile});
    await again.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    await again.navigate(`${server.url}/`);
    await again.waitFor(`getComputedStyle(document.getElementById('first-run')).display !== 'none'`, {timeout: 20000});
    await again.evaluate(`import('./first-run.mjs').then(({firstRun}) => { firstRun.offerAgentSignIn(async () => ${FAKE_CODE}); })`);
    const button = await again.evaluate(`(() => { const r = document.getElementById('first-run-agent-sign-in').getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2}; })()`);
    for (const type of ['mousePressed', 'mouseReleased']) {
      await again.send('Input.dispatchMouseEvent', {type, x: button.x, y: button.y, button: 'left', clickCount: 1});
    }
    await again.waitFor(`document.getElementById('first-run-prompt').textContent.includes('tilo login')`, {timeout: 10000});
    await again.evaluate(`globalThis.polled = {resolved: true, expired: false, steps: ['resolved'], claimed: {user: '${'ab'.repeat(32)}', invite: '${'cd'.repeat(16)}', by: 'Claude Code on build-box'}}`);
    await again.waitFor(`document.getElementById('first-run-steps').textContent.includes('vouched for you')`, {timeout: 5000});
    check(await again.evaluate(`document.getElementById('first-run-prompt').textContent.startsWith('Sign me in to Tilo: run tilo login K7Q-MX2P')`),
      `${name}: the returning prompt is the short sign-in one`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-returning.png`, await again.screenshot());
    await again.close();
  }
}

await browser.close();
await server.close();
if (failures.length) {
  console.error(`${failures.length} failed`);
  process.exit(1);
}
