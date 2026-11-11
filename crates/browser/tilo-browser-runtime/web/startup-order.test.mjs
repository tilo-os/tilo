// The order the entry pages start the runtime in: everything a first visit's
// desktop needs downloads and compiles while something else waits, never
// one after the other.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

const web = new URL('./', import.meta.url);

const settle = async () => { for (let i = 0; i < 20; i += 1) await new Promise(resolve => setImmediate(resolve)); };

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
