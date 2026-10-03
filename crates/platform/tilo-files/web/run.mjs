// tilo-files in headless Chrome: builds `examples/web_probe.rs` for the web,
// serves it and checks OPFS and Web Locks through the crate's API.
//
// usage: node crates/platform/tilo-files/web/run.mjs [--no-build]
//
// Phases: the conformance suite; files written before a reload read back
// after it, and a lock the page never released is free again; a lock held
// in one tab makes another wait, and closing the holder's tab frees it.
import {execFileSync} from 'node:child_process';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const repo = join(dirname(fileURLToPath(import.meta.url)), '../../../..');
const build = !process.argv.includes('--no-build');
const out = mkdtempSync(join(tmpdir(), 'tilo-files-web-'));

const cargo = (...args) => execFileSync('cargo', args, {cwd: repo, stdio: ['ignore', 'pipe', 'inherit']}).toString();
if (build) {
  execFileSync('cargo', ['build', '-p', 'tilo-files', '--example', 'web_probe', '--target', 'wasm32-unknown-unknown'],
    {cwd: repo, stdio: 'inherit'});
}
const target = JSON.parse(cargo('metadata', '--format-version', '1', '--no-deps', '--offline')).target_directory;
execFileSync('wasm-bindgen', [
  join(target, 'wasm32-unknown-unknown/debug/examples/web_probe.wasm'),
  '--target', 'web', '--out-dir', out, '--out-name', 'probe',
], {stdio: 'inherit'});
writeFileSync(join(out, 'index.html'), `<!doctype html><meta charset="utf-8"><script type="module">
import init, * as probe from './probe.js';
await init();
globalThis.probe = probe;
globalThis.probeReady = true;
</script>`);

const server = await serve(out);
const browser = await launch();
const failures = [];
const expect = (name, ok, detail = '') => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ` ${detail}` : ''}`);
  if (!ok) failures.push(name);
};
const opened = async () => {
  const tab = await browser.tab();
  await tab.navigate(`${server.url}/index.html`);
  await tab.waitFor('globalThis.probeReady === true');
  return tab;
};
const reported = (phase, report) => {
  for (const name of report.passed) expect(`${phase}: ${name}`, true);
  for (const {name, error} of report.failed) expect(`${phase}: ${name}`, false, error);
};

try {
  const a = await opened();
  reported('suite', await a.evaluate(`probe.run_suite('suite-${Date.now()}')`));

  const mirrorSuite = `mirror-${Date.now()}`;
  reported('mirror', await a.evaluate(`probe.run_mirror_suite('${mirrorSuite}')`));
  await a.evaluate(`probe.cleanup('${mirrorSuite}')`);

  const profile = `reload-${Date.now()}`;
  const mirrored = `reload-mirror-${Date.now()}`;
  reported('before reload', await a.evaluate(`probe.seed('${profile}')`));
  reported('before reload', await a.evaluate(`probe.seed_mirror('${mirrored}')`));
  await a.reload();
  await sleep(300);
  await a.waitFor('globalThis.probeReady === true');
  reported('after reload', await a.evaluate(`probe.verify('${profile}')`));
  reported('after reload', await a.evaluate(`probe.verify_mirror('${mirrored}')`));
  await a.evaluate(`probe.cleanup('${mirrored}')`);

  const tabs = `tabs-${Date.now()}`;
  const b = await opened();
  // A writes through its mirror; B's watch reports it and B's mirror has it.
  reported('watcher', await b.evaluate(`probe.watch_start('${tabs}')`));
  reported('writer', await a.evaluate(`probe.write_through('${tabs}', 'shared.json', 'from A')`));
  let seen = [];
  for (let i = 0; i < 40 && !seen.length; i++) {
    await sleep(50);
    seen = await b.evaluate('probe.watch_take()');
  }
  expect('another tab\'s watch sees a write', seen.includes('Changed shared.json'), JSON.stringify(seen));
  const mirroredThere = await b.evaluate(`probe.read_mirrored('${tabs}', 'shared.json')`);
  expect('another tab\'s mirror has the write', mirroredThere === 'from A', JSON.stringify(mirroredThere));

  // A holds; B waits until A releases.
  reported('holder', await a.evaluate(`probe.acquire('${tabs}', 'one.lock')`));
  const waiting = b.evaluate(`probe.wait_for('${tabs}', 'one.lock', true, 10000)`);
  await sleep(1000);
  await a.evaluate(`probe.release('one.lock')`);
  const waited = await waiting;
  expect('another tab waits for an exclusive lock until release', waited >= 900 && waited < 10000, `(${Math.round(waited)} ms)`);
  const free = await b.evaluate(`probe.wait_for('${tabs}', 'one.lock', false, 1000)`);
  expect('a released lock is free in another tab', free >= 0 && free < 500, `(${Math.round(free)} ms)`);

  // A holds and its tab closes: B gets the lock at once.
  reported('holder', await a.evaluate(`probe.acquire('${tabs}', 'closed.lock')`));
  const busy = await b.evaluate(`probe.wait_for('${tabs}', 'closed.lock', false, 300)`);
  expect('a lock held in another tab is busy', busy === -1, `(${busy})`);
  await a.close();
  const freed = await b.evaluate(`probe.wait_for('${tabs}', 'closed.lock', false, 5000)`);
  expect('closing the holder\'s tab frees its lock', freed >= 0 && freed < 5000, `(${Math.round(freed)} ms)`);
  await b.evaluate(`probe.cleanup('${tabs}')`);

  for (const line of [...a.log, ...b.log].filter(l => l.startsWith('EXC') || /panic/i.test(l))) {
    expect('no exceptions', false, line);
  }
} catch (error) {
  expect('driver', false, error.stack ?? String(error));
} finally {
  await browser.close();
  await server.close();
  rmSync(out, {recursive: true, force: true});
}
console.log(failures.length ? `${failures.length} failed` : 'all passed');
process.exit(failures.length ? 1 : 0);
