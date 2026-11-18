import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const here = dirname(fileURLToPath(import.meta.url));

/** The served URL of `serve.mjs` over `root`, and its process. */
async function started(root) {
  const child = spawn(process.execPath, [join(here, 'serve.mjs'), root, '0'], {stdio: ['ignore', 'pipe', 'inherit']});
  const url = await new Promise((done, fail) => {
    let out = '';
    child.stdout.on('data', chunk => {
      out += chunk;
      const line = out.split('\n').find(text => text.startsWith('http://'));
      if (line) done(line.trim());
    });
    child.on('exit', code => fail(new Error(`serve.mjs exited with ${code}`)));
  });
  return {url, child};
}

// A person's browser keeps what a server lets it: a bundle rebuilt under the
// same names reached a browser that had shown the last one as old modules
// beside new ones, and the page stopped answering presses. What the
// tilolang page is served with must reach the next load whole.
test('the page server lets no browser keep a file, and serves a rebuilt one at once', async () => {
  const root = mkdtempSync(join(tmpdir(), 'tilo-serve-'));
  const {url, child} = await started(root).catch(error => { rmSync(root, {recursive: true, force: true}); throw error; });
  try {
    const file = join(root, 'page.mjs');
    writeFileSync(file, 'export const build = 1;\n');
    let response = await fetch(`${url}/page.mjs`);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(response.headers.get('content-type'), 'text/javascript');
    assert.equal(await response.text(), 'export const build = 1;\n');
    writeFileSync(file, 'export const build = 22;\n');
    utimesSync(file, new Date(), new Date(Date.now() + 1000));
    response = await fetch(`${url}/page.mjs`);
    assert.equal(await response.text(), 'export const build = 22;\n');
    for (const [name, type] of [['app.wasm', 'application/wasm'], ['icon.svg', 'image/svg+xml'], ['en.ftl', 'text/plain; charset=utf-8']]) {
      writeFileSync(join(root, name), 'x');
      response = await fetch(`${url}/${name}`);
      assert.equal(response.headers.get('content-type'), type, name);
      assert.equal(response.headers.get('cache-control'), 'no-store', name);
    }
    assert.equal((await fetch(`${url}/missing.mjs`)).status, 404);
  } finally {
    child.kill('SIGTERM');
    rmSync(root, {recursive: true, force: true});
  }
});

test('tilolang-web.sh serves through the page server', () => {
  const script = readFileSync(resolve(here, 'tilolang-web.sh'), 'utf8');
  assert.match(script, /exec node tools\/web\/html-renderer\/serve\.mjs /);
  assert.doesNotMatch(script, /http\.server/);
});
