// The order the entry pages start the runtime in: everything a first visit's
// desktop needs downloads and compiles while something else waits, never
// one after the other.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

const web = new URL('./', import.meta.url);
const read = path => readFileSync(new URL(path, web), 'utf8');
const compileScript = read('compile.inline.js');
const scrubber = read('scrub.inline.js');

test('every entry page starts compiling the runtime module right after the scrubber, before any module', () => {
  for (const page of ['index.html', 'app/index.html']) {
    const html = read(page);
    const inlined = `<script>\n${scrubber}</script>\n<script>\n${compileScript}</script>`;
    assert.ok(html.includes(inlined), `${page} must inline compile.inline.js unchanged after the scrubber`);
    const at = html.indexOf(inlined);
    assert.ok(html.indexOf('tilo_browser_runtime_bg.wasm" as="fetch" crossorigin>') < at, `${page}: the preload it takes comes first`);
    assert.ok(html.indexOf('<script type="module"') > at, `${page}: no module before it`);
  }
});

/** Run the inline script on a page holding `href` as the runtime's preload. */
function runCompile({href = 'https://tilo.example/v/1/pkg/tilo_browser_runtime_bg.wasm', streaming = true} = {}) {
  const fetched = [];
  const compiled = [];
  const context = {
    document: {querySelector: selector => (selector.includes('tilo_browser_runtime_bg.wasm') && href ? {href} : null)},
    fetch: (url, init) => { fetched.push([url, init]); return Promise.resolve({url}); },
    WebAssembly: {compileStreaming: streaming ? response => { compiled.push(response); return Promise.resolve({module: true}); } : undefined},
  };
  context.globalThis = context;
  runInNewContext(compileScript, context);
  return {context, fetched, compiled};
}

test('the inline script compiles the preloaded module as it streams, with the preload’s own request', async () => {
  const {context, fetched, compiled} = runCompile();
  assert.deepEqual(fetched.map(([url]) => url), ['https://tilo.example/v/1/pkg/tilo_browser_runtime_bg.wasm']);
  // The preload is `as="fetch" crossorigin`: a fetch the same way takes its response.
  assert.deepEqual({...fetched[0][1]}, {mode: 'cors', credentials: 'same-origin'});
  assert.equal(compiled.length, 1, 'compiled from the response, not after reading it whole');
  assert.deepEqual({...await context.__tiloStartModule}, {module: true});
});

test('without streaming compilation or the preload, the page leaves the runtime to init', () => {
  assert.equal(runCompile({streaming: false}).context.__tiloStartModule, undefined);
  assert.equal(runCompile({href: null}).context.__tiloStartModule, undefined);
});

/** `host.mjs` with its wasm bindings and the splitter's loader replaced by
 * the fakes on `globalThis.__startupFixture`, everything else real. */
async function loadHost() {
  let source = read('host.mjs').replaceAll('import.meta.url', JSON.stringify(new URL('./host.mjs', web).href));
  source = source
    .replace("import init,* as runtime from './pkg/tilo_browser_runtime.js';",
      'const init=(...args)=>globalThis.__startupFixture.init(...args);const runtime={};')
    .replaceAll("import('./pkg/__wasm_split.js')", 'globalThis.__startupFixture.split()')
    .replace(/from '(\.\/[^']+)'/g, (_all, path) => `from '${new URL(path, web).href}'`);
  return import(`data:text/javascript;base64,${Buffer.from(`${source}\n// ${Math.random()}`).toString('base64')}`);
}

const settle = async () => { for (let i = 0; i < 20; i += 1) await new Promise(resolve => setImmediate(resolve)); };

/** A fixture whose runtime becomes ready only when `release()` is called. */
function fixture({startModule} = {}) {
  const steps = [];
  let release;
  const ready = new Promise(resolve => { release = resolve; });
  const split = {
    prefetch: name => { steps.push(`prefetch ${name}`); return Promise.resolve(true); },
    load: async name => { steps.push(`load ${name}`); },
  };
  globalThis.__startupFixture = {
    split: async () => split,
    init: async options => { steps.push(`init ${options?.module_or_path ? 'compiled' : 'fetch'}`); await ready; steps.push('ready'); },
  };
  if (startModule === undefined) delete globalThis.__tiloStartModule;
  else globalThis.__tiloStartModule = startModule;
  return {steps, release: () => release()};
}

test('the peer’s code is fetched and compiled while the start module still compiles, and init takes the page’s compile', async () => {
  const {steps, release} = fixture({startModule: Promise.resolve({compiled: true})});
  const host = await loadHost();
  await settle();
  // Before the runtime is ready: the peer's module is already asked for.
  assert.deepEqual([...steps].sort(), ['init compiled', 'prefetch peer']);
  release();
  const runtime = host.loadRuntime();
  await settle();
  assert.deepEqual(steps.slice(2), ['ready', 'load peer']);
  await runtime;
  delete globalThis.__tiloStartModule;
});

test('a page whose early compile failed has init fetch the module itself', async () => {
  const failed = Promise.reject(new Error('not wasm'));
  failed.catch(() => {});
  const {steps, release} = fixture({startModule: failed});
  await loadHost();
  await settle();
  assert.deepEqual([...steps].sort(), ['init fetch', 'prefetch peer']);
  release();
});

test('a split module downloads beside the shared code it needs, not after it', async () => {
  const source = readFileSync(new URL('../../../../tools/web/wasm-split/src/main.rs', web), 'utf8');
  const template = /const LOADER: &str = r#"([\s\S]*?)"#;/.exec(source)?.[1];
  assert.ok(template, 'the loader template is where the test reads it');
  const fetched = [];
  const pending = new Map();
  const context = {
    URL,
    console,
    fetch: url => { const name = String(url).split('/').pop(); fetched.push(name); return new Promise(resolve => pending.set(name, resolve)); },
    WebAssembly: {
      compileStreaming: async response => response,
      instantiate: async () => ({exports: {}}),
    },
  };
  context.globalThis = context;
  const body = template
    .replace(/^import \{initSync\} from '\.\/tilo_browser_runtime\.js';$/m,
      'const initSync = () => ({memory: {}, __indirect_function_table: {get: () => () => {}}});')
    .replaceAll('import.meta.url', '"https://tilo.example/pkg/__wasm_split.js"')
    .replace(/^export /gm, '');
  runInNewContext(`${body}
    const shared = loader('shared0.wasm', []);
    globalThis.chat = loader('chat.wasm', [shared]);`, context);
  void context.chat.load();
  await settle();
  assert.deepEqual(fetched.sort(), ['chat.wasm', 'shared0.wasm'], 'both asked for at once');
  void context.chat.prefetch();
  for (const resolve of pending.values()) resolve({ok: true});
  await settle();
  assert.equal(fetched.length, 2, 'each module once');
});
