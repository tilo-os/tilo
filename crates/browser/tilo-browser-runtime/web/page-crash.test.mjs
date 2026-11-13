// The page host's own reaction to a panic that named no window: the first one
// starts the page over, and one that lands in the same crash leaves the screen
// up instead. Each import is a page load; the reload itself is counted.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

const settle = ms => new Promise(resolve => setTimeout(resolve, ms));

class Node {
  constructor(tag) { this.tagName = tag; this.children = []; this.attributes = {}; this.style = {}; this.textContent = ''; }
  append(...nodes) { for (const node of nodes) { node.parentNode = this; this.children.push(node); } }
  setAttribute(name, value) { this.attributes[name] = value; }
  addEventListener(type, listener) { this.listener = {type, listener}; }
  focus() { this.focused = true; }
}

function memoryStorage() {
  const values = new Map();
  return {getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, String(value)), removeItem: key => values.delete(key)};
}

/** One page load of the real host module: only the wasm module, storage and
 * the page's own elements are stand-ins. */
async function loadPage({storage, reports = []}) {
  const document = {head: new Node('head'), body: new Node('body'), createElement: tag => new Node(tag)};
  const reloads = [];
  globalThis.document = document;
  globalThis.localStorage = storage;
  globalThis.location = {pathname: '/', reload: () => reloads.push('reload')};
  let source = (await readFile(new URL('./host.mjs', import.meta.url), 'utf8'))
    .replaceAll('import.meta.url', JSON.stringify(new URL('./host.mjs', import.meta.url).href));
  source = source
    .replace("import init,* as runtime from './pkg/tilo_browser_runtime.js';", 'const init=async()=>{};const runtime={};')
    .replace("import {acquireStorage} from './storage.mjs';", 'const acquireStorage=async()=>{throw Error("no storage in this test");};')
    .replace(/from '(\.\/[^']+)'/g, (_all, path) => `from '${new URL(path, import.meta.url).href}'`);
  globalThis.fetch = async () => ({ok: false, status: 404, json: async () => ({})});
  const host = await import(`data:text/javascript;base64,${Buffer.from(`${source}\n// ${Math.random()}`).toString('base64')}`);
  host.reportPageErrors((kind, message) => reports.push({kind, message}));
  return {host, document, reloads};
}

test('a crash outside any window starts the page over, and a second one leaves the screen up', async () => {
  const storage = memoryStorage();
  const reports = [];
  const crash = '{"tab":null,"message":"already borrowed: BorrowMutError"}';
  const first = await loadPage({storage, reports});
  globalThis.__tiloRuntimePanic(crash);
  await settle(350);
  assert.deepEqual(first.reloads, ['reload'], 'the page starts itself over');
  assert.equal(first.document.body.children.length, 0, 'and leaves no screen behind');
  assert.deepEqual(reports, [{kind: 'panic', message: 'outside any window: already borrowed: BorrowMutError'}],
    'the door hears about it before the reload');

  // The page that load started crashes again, inside the bound.
  const second = await loadPage({storage, reports});
  globalThis.__tiloRuntimePanic(crash);
  await settle(350);
  assert.deepEqual(second.reloads, [], 'a load that just started over does not do it again');
  const [screen] = second.document.body.children;
  assert.equal(screen.className, 'tilo-page-crash');
  assert.equal(screen.attributes.role, 'alert');
  const [title, said, button] = screen.children[0].children;
  assert.equal(title.textContent, 'Tilo hit a problem');
  assert.equal(said.textContent, 'already borrowed: BorrowMutError');
  assert.equal(button.textContent, 'Reload');
  assert.equal(button.focused, true);
  button.listener.listener();
  assert.deepEqual(second.reloads, ['reload'], 'and the button is the way on');
  assert.equal(reports.length, 2, 'both crashes reach the door, swallowed by neither');
});

test('a crash inside a window is left to that window', async () => {
  const storage = memoryStorage();
  const page = await loadPage({storage});
  globalThis.__tiloRuntimePanic('{"tab":"7","message":"boom"}');
  await settle(350);
  assert.deepEqual(page.reloads, []);
  assert.equal(page.document.body.children.length, 0);
  assert.equal(storage.getItem('tilo.page-restarts.v1'), null, 'no page restart is recorded for it');
});

test('a Wasm trap no panic preceded, such as memory running out, starts the page over too', async () => {
  const storage = memoryStorage();
  const reports = [];
  const listeners = [];
  globalThis.addEventListener = (type, listener) => { if (type === 'error') listeners.push(listener); };
  try {
    const page = await loadPage({storage, reports});
    const trap = new WebAssembly.RuntimeError('unreachable');
    Object.defineProperty(trap, 'stack', {value: 'RuntimeError: unreachable\n    at x.wasm._RNvNtCs0_3std5alloc8rust_oom'});
    listeners.at(-1)({error: trap});
    await settle(350);
    assert.deepEqual(page.reloads, ['reload'], 'a page stalled on a dead module starts over');
    assert.deepEqual(reports, [{kind: 'panic', message: 'outside any window: the page ran out of memory'}]);
  } finally {
    delete globalThis.addEventListener;
  }
});
