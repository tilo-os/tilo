// Build web_custody with rcargo first. This runner invokes no Cargo or metadata.
// Production FilesBackend in a fresh Chrome profile; all content is synthetic.
import {execFileSync} from 'node:child_process';
import {copyFileSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {launch, serve} from '../../../../tools/web/chrome.mjs';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');
const out = mkdtempSync(join(tmpdir(), 'tilo-content-custody-'));
let server;
let browser;
const expect = (name, ok) => {
  if (!ok) throw new Error(name);
  console.log(`ok ${name}`);
};
try {
  const prebuilt = process.argv.indexOf('--prebuilt');
  if (prebuilt !== -1) {
    if (!process.argv[prebuilt + 1]) throw new Error('--prebuilt needs an artifact directory');
    const artifacts = resolve(process.argv[prebuilt + 1]);
    for (const file of ['probe.js', 'probe_bg.wasm']) copyFileSync(join(artifacts, file), join(out, file));
  } else {
    execFileSync('wasm-bindgen', [
      join(repo, 'target/wasm32-unknown-unknown/debug/examples/web_custody.wasm'),
      '--target', 'web', '--out-dir', out, '--out-name', 'probe',
    ], {stdio: 'inherit'});
  }
  writeFileSync(join(out, 'index.html'), `<!doctype html><script type="module">
import init, * as probe from './probe.js';
await init(); globalThis.probe = probe; globalThis.probeBoot = crypto.randomUUID(); globalThis.probeReady = true;
</script>`);
  server = await serve(out);
  browser = await launch();
  const tab = await browser.tab();
  await tab.navigate(`${server.url}/index.html`);
  await tab.waitFor('globalThis.probeReady === true');
  const p = JSON.stringify(`custody-${Date.now()}`);
  const seed = JSON.stringify(Array.from(new TextEncoder().encode('reviewed image bytes')));
  const failed = JSON.stringify(Array.from(new TextEncoder().encode('second reviewed image')));
  await tab.evaluate(`probe.prepare(${p})`);
  const hash = await tab.evaluate(`probe.put(${p},new Uint8Array(${seed}))`);
  const h = JSON.stringify(hash);
  await tab.evaluate(`probe.flush(${p})`);
  expect('closed OPFS bytes attest their exact address', await tab.evaluate(`probe.custody(${p},${h})`) === 'Durable');
  expect('another profile cannot attest the same address', await tab.evaluate(`probe.custody(${JSON.stringify(`custody-other-${Date.now()}`)},${h})`) === 'Absent');
  const boot = JSON.stringify(await tab.evaluate('globalThis.probeBoot'));
  await tab.reload();
  // Page.reload acknowledges navigation before the old context is gone.
  await tab.waitFor(`globalThis.probeReady === true && globalThis.probeBoot !== ${boot}`);
  expect('custody survives reload before loading a mirror', await tab.evaluate(`probe.custody(${p},${h})`) === 'Durable');
  await tab.evaluate(`probe.prepare(${p})`);
  await tab.evaluate(`globalThis.originalWrite = FileSystemFileHandle.prototype.createWritable;
    FileSystemFileHandle.prototype.createWritable = function() {
      return Promise.reject(new DOMException('synthetic write failure','UnknownError'));
    };`);
  const retryHash = await tab.evaluate(`probe.put(${p},new Uint8Array(${failed}))`);
  const r = JSON.stringify(retryHash);
  expect('storage failure is observable', await tab.evaluate(`probe.flush(${p}).then(()=>false,()=>true)`));
  expect('failed bytes remain in the mirror', await tab.evaluate(`Array.from(probe.mirrored(${p},${r})).toString() === ${failed}.toString()`));
  // Creating the handle may have committed an empty file before the stream
  // failed. Both absent bytes and that exact hash mismatch must be refused.
  expect('cleared global failure cannot attest failed bytes', await tab.evaluate(`probe.custody(${p},${r}).then(x=>x==='Absent',e=>String(e).includes('does not match'))`));
  expect('another committed address remains durable', await tab.evaluate(`probe.custody(${p},${h})`) === 'Durable');
  expect('awaited write reports its own failure', await tab.evaluate(`probe.put_committed(${p},new Uint8Array(${failed})).then(()=>false,()=>true)`));
  expect('existing durable bytes cannot hide an awaited write failure', await tab.evaluate(`probe.put_committed(${p},new Uint8Array(${seed})).then(()=>false,()=>true)`));
  await tab.evaluate('FileSystemFileHandle.prototype.createWritable = originalWrite');
  expect('awaited retry keeps the same content address', await tab.evaluate(`probe.put_committed(${p},new Uint8Array(${failed}))`) === retryHash);
  expect('retry commits despite mirror presence', await tab.evaluate(`probe.custody(${p},${r})`) === 'Durable');
  await tab.evaluate(`await (async () => { globalThis.data = await navigator.storage.getDirectory();
    data = await data.getDirectoryHandle(${p}); data = await data.getDirectoryHandle('probe');
    data = await data.getDirectoryHandle('data');
    let file = await data.getFileHandle(${h}); let writer = await file.createWritable();
    await writer.write('tampered'); await writer.close(); })();`);
  expect('corruption refuses a valid mirror', await tab.evaluate(`probe.custody(${p},${h}).then(()=>false,e=>String(e).includes('does not match'))`));
  await tab.evaluate(`await data.removeEntry(${r})`);
  expect('deleted persisted bytes are absent despite mirror presence', await tab.evaluate(`probe.custody(${p},${r})`) === 'Absent');
  await tab.evaluate(`await (async () => { let file = await data.getFileHandle(${h}); let writer = await file.createWritable();
    await writer.write(new Uint8Array(129)); await writer.close(); })();`);
  expect('oversized content refuses bounded readback', await tab.evaluate(`probe.custody(${p},${h}).then(()=>false,e=>String(e).includes('readback limit'))`));
  expect('invalid address is refused', await tab.evaluate(`probe.custody(${p},'../private').then(()=>false,()=>true)`));
  await tab.evaluate(`const root = await navigator.storage.getDirectory(); await root.removeEntry(${p},{recursive:true});`);
} finally {
  try {
    if (browser) await browser.close();
  } finally {
    try {
      if (server) await server.close();
    } finally {
      rmSync(out, {recursive: true, force: true});
    }
  }
}
