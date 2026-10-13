// An idle desktop presents no frames. Opens a room's worth of windows,
// leaves them alone, and counts the frames presented over a few seconds; the
// only ones allowed are scheduled ticks (Clocks' second hand, once a second).
// Each presented frame is attributed to what asked for it (the runtime's
// frame metrics: input, a component's wake, a widget's animation or deadline,
// by widget path), so a failure names the offender.
//
// usage: node idle.mjs BUNDLE_DIR [--webkit] [--stall-network] [--seconds N] [--max-fps F] [--apps ID,...]
//   BUNDLE_DIR       build.sh --bundle desktop output
//   --apps           the windows to open (default: Chat, API Tester, Word
//                    Processor, Sheets, Notes, Maps, Calendar, Photos, Clocks,
//                    Slides)
//   --webkit         Playwright's WebKit instead of headless Chrome; resolves
//                    playwright-core from $PLAYWRIGHT_CORE, else from this tree.
//                    Also reports the GPU process's memory while idle.
//   --stall-network  requests to other origins never answer (Maps' tiles and
//                    place searches stay pending), as on a failing network
//   --seconds        idle window measured (default 3)
//   --max-fps        presented frames per second allowed (default 2: Clocks'
//                    tick with slack)
// Prints one JSON line per engine run; exits non-zero when the idle desktop
// presents more than that.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const args = process.argv.slice(2);
const option = (name, fallback) => { const i = args.indexOf(name); return i < 0 ? fallback : args[i + 1]; };
const bundle = args.find((arg, i) => !arg.startsWith('--') && !['--seconds', '--max-fps', '--apps'].includes(args[i - 1]));
if (!bundle) throw Error('usage: node idle.mjs BUNDLE_DIR [--webkit] [--stall-network] [--seconds N] [--max-fps F] [--apps ID,...]');
const webkit = args.includes('--webkit');
const stall = args.includes('--stall-network');
const seconds = Number(option('--seconds', 3));
const maxFps = Number(option('--max-fps', 2));
const APPS = option('--apps', ['so.tilo.chat', 'so.tilo.api-tester', 'so.tilo.word-processor', 'so.tilo.sheets', 'so.tilo.notes',
  'so.tilo.maps', 'so.tilo.calendar', 'so.tilo.photos', 'so.tilo.clocks', 'so.tilo.slides'].join(',')).split(',');
const server = await serve(resolve(bundle));
const local = new URL(server.url).origin;

async function chrome() {
  const browser = await launch();
  const tab = await browser.tab();
  // Paused and never continued: every request to another origin (the bundle
  // is served over plain HTTP, the apps' services over HTTPS) pends.
  if (stall) await tab.send('Fetch.enable', {patterns: [{urlPattern: 'https://*'}]});
  await tab.navigate(`${server.url}/`);
  return {evaluate: tab.evaluate, errors: () => tab.log.filter(line => line.startsWith('EXC')), close: () => browser.close(),
    memory: () => null};
}
async function playwrightWebkit() {
  const core = process.env.PLAYWRIGHT_CORE ?? 'playwright-core';
  const {webkit: engine} = await import(core.startsWith('/') ? pathToFileURL(resolve(core, 'index.mjs')).href : core);
  const profile = mkdtempSync(join(tmpdir(), 'tilo-webkit-'));
  const context = await engine.launchPersistentContext(profile, {viewport: {width: 1280, height: 800}});
  const page = context.pages()[0] ?? await context.newPage();
  if (stall) await page.route(url => url.origin !== local, () => {});
  const errors = [];
  page.on('pageerror', error => errors.push(`EXC ${error.message}`));
  await page.goto(`${server.url}/`);
  return {evaluate: expression => page.evaluate(expression), errors: () => errors,
    close: async () => { await context.close(); rmSync(profile, {recursive: true, force: true}); },
    memory: webkitMemory};
}

// The resident and physical footprint of Playwright's WebKit processes (its
// GPU, web content and network processes are XPC services inside its build),
// in MiB. Another Playwright WebKit running at the same time is counted too.
function webkitMemory() {
  const table = execFileSync('ps', ['-Ao', 'pid=,ppid=,rss=,command='], {encoding: 'utf8'}).trim().split('\n')
    .map(line => line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/)).filter(Boolean)
    .map(([, pid, ppid, rss, command]) => ({pid: +pid, ppid: +ppid, rss: +rss, command}));
  const ours = new Set(table.filter(p => /ms-playwright\/webkit/.test(p.command)).map(p => p.pid));
  for (let grew = true; grew; ) {
    grew = false;
    for (const p of table) if (!ours.has(p.pid) && ours.has(p.ppid)) { ours.add(p.pid); grew = true; }
  }
  const footprint = pid => {
    try {
      const out = execFileSync('footprint', ['-p', String(pid)], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore']});
      const m = out.match(/Footprint:\s+([\d.]+)\s+(KB|MB|GB)/);
      return m ? +m[1] * {KB: 1 / 1024, MB: 1, GB: 1024}[m[2]] : null;
    } catch { return null; }
  };
  const kinds = {gpu: /WebKit\.GPU/, webContent: /WebKit\.WebContent/, networking: /WebKit\.Networking/, browser: /Playwright\.app/};
  const memory = {};
  for (const p of table.filter(p => ours.has(p.pid))) {
    const kind = Object.keys(kinds).find(k => kinds[k].test(p.command.split(' ')[0]));
    if (!kind) continue;
    const entry = memory[kind] ??= {processes: 0, rssMiB: 0, footprintMiB: 0};
    entry.processes++;
    entry.rssMiB += Math.round(p.rss / 1024);
    entry.footprintMiB += Math.round(footprint(p.pid) ?? 0);
  }
  return memory;
}

const page = webkit ? await playwrightWebkit() : await chrome();
const {evaluate} = page;
for (let i = 0; ; i++) {
  await sleep(1000);
  if (await evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
  assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
}
const command = body => evaluate(`(async()=>{const s=tiloMount.navigation.getSnapshot();await tiloMount.navigation.dispatch({...${JSON.stringify(body)},generation:s.generation});return true;})()`);
const available = (await evaluate('tiloMount.navigation.getSnapshot()')).applications.map(a => a.id);
const opened = [];
for (const app of APPS) {
  if (!available.includes(app)) continue;
  await command({kind: 'launch', app});
  opened.push(app);
  await sleep(1200);
}
// Let decodes, fetches that do answer, and opening animations finish.
await sleep(5000);

const runtime = `(await import(new URL('pkg/tilo_browser_runtime.js', document.baseURI).href))`;
await evaluate(`(async()=>{const r=${runtime};r.configure_frame_metrics(true);r.take_frame_metrics();return true;})()`);
const measured = await evaluate(`(async()=>{
  const frames=()=>tiloMount.state()?.frames??0,start=performance.now(),from=frames();
  await new Promise(r=>setTimeout(r,${seconds * 1000}));
  return {presented:frames()-from,seconds:(performance.now()-start)/1000};
})()`);
const memory = page.memory();
const metrics = JSON.parse(await evaluate(`(async()=>{const r=${runtime};const m=r.take_frame_metrics();r.configure_frame_metrics(false);return m;})()`));
const requesters = {};
for (const mount of metrics) for (const [who, count] of Object.entries(mount.requesters ?? {})) requesters[who] = (requesters[who] ?? 0) + count;
const presentedPerSecond = +(measured.presented / measured.seconds).toFixed(2);
const result = {engine: webkit ? 'webkit' : 'chrome', stallNetwork: stall, windows: opened.length, ...measured, presentedPerSecond,
  requesters: Object.fromEntries(Object.entries(requesters).sort((a, b) => b[1] - a[1])),
  ...(memory ? {memory} : {}), errors: page.errors().slice(0, 5)};
console.log(JSON.stringify(result));
await page.close();
await server.close();
assert.ok(presentedPerSecond <= maxFps, `an idle desktop presented ${presentedPerSecond} frames/s (at most ${maxFps}): ${JSON.stringify(result.requesters)}`);
