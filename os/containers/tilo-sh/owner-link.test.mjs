// The page `stack.sh owner` prints: what it serves, and that one key it takes.
//
// The page itself runs in the person's browser, which this test does not
// drive; what it checks is the helper's side of it: the page under the token
// and nothing else, the key kept once and for the stack to read, and the
// content type that keeps another origin's page from posting one.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {existsSync, mkdtempSync, readFileSync, rmSync, statSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {after, before, test} from 'node:test';

const here = new URL('.', import.meta.url).pathname;
const token = 'a'.repeat(32);
const state = mkdtempSync(join(tmpdir(), 'owner-link-'));
const key = 'b'.repeat(64);
let child;
let origin;

const wait = {
  before: async () => {
    child = spawn(process.execPath, [join(here, 'owner-link.mjs'), state, '0', token], {
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    origin = await new Promise((resolve, reject) => {
      let seen = '';
      child.stdout.on('data', chunk => {
        seen += chunk;
        const line = seen.split('\n').find(line => line.includes('"url"'));
        if (line) resolve(JSON.parse(line).url.replace(`/owner/${token}`, ''));
      });
      child.on('exit', code => reject(new Error(`the helper exited with ${code}`)));
    });
  },
  after: async () => {
    child?.kill();
    rmSync(state, {recursive: true, force: true});
  },
};

before(wait.before);
after(wait.after);

test('the page under the token is served, and nothing else is', async () => {
  const page = await fetch(`${origin}/owner/${token}`);
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.match(html, /Make this browser the Lounge's owner/, 'the page says what it does');
  assert.match(html, /localStorage/, 'it reads the key the browser made');
  assert.match(html, /tilo-own-workspace-named:/, 'out of the workspace it named on this host');
  assert.match(html, /tilo-host-memberships/, 'else the user a joined host names');
  assert.match(html, /tilo-stand-in:/, 'else the identity a stand-in key names');
  assert.equal((await fetch(`${origin}/owner/`)).status, 404);
  assert.equal((await fetch(`${origin}/owner/defaced`)).status, 404);
  assert.equal((await fetch(`${origin}/`)).status, 404);
});

test('another origin\'s form posts no key, and no key that is not one is taken', async () => {
  // A page elsewhere posts a body a browser sends without asking: it is not the site's page.
  const form = await fetch(`${origin}/owner/${token}`, {
    method: 'POST',
    headers: {'content-type': 'text/plain'},
    body: JSON.stringify({key}),
  });
  assert.equal(form.status, 404, 'a plain-text body is no key of ours');
  for (const body of [{key: 'not a key'}, {key: 'b'.repeat(63)}, {nothing: true}, {}]) {
    const response = await fetch(`${origin}/owner/${token}`, {
      method: 'POST',
      headers: {'content-type': 'application/json'},
      body: JSON.stringify(body),
    });
    assert.equal(response.status, 400, `${JSON.stringify(body)} is not a user key`);
  }
  assert.equal(existsSync(join(state, `${token}.key`)), false, 'nothing was kept');
});

test('the key the page read is kept, once, and the page stops', async () => {
  const response = await fetch(`${origin}/owner/${token}`, {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({key}),
  });
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), {declared: true});
  const kept = join(state, `${token}.key`);
  const deadline = Date.now() + 5000;
  while (!existsSync(kept) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(readFileSync(kept, 'utf8').trim(), key);
  assert.equal(statSync(kept).mode & 0o777, 0o600, 'only the person running the stack reads it');
  // The link is spent: the helper stops, as one whose key the stack has.
  const exited = await new Promise(resolve => {
    if (child.exitCode !== null) return resolve(true);
    child.on('exit', () => resolve(true));
    setTimeout(() => resolve(false), 5000);
  });
  assert.equal(exited, true, 'the page stopped after taking the key');
});
