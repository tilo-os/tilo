import test from 'node:test';
import assert from 'node:assert/strict';
import {readSource, runtimeSurface, pageModules, runtimeFake, unexportedCalls, withoutComments} from './runtime-surface.mjs';

const surface = runtimeSurface();

test('every page module calls only what the runtime exports', () => {
  const unexported = pageModules().flatMap(page =>
    unexportedCalls(surface, page.source).map(call => {
      const named = call.type === null ? `runtime.${call.name}` : `runtime.${call.type}.${call.name}`;
      return `${page.name}:${call.line}: ${named}(…) — the runtime exports no ${call.type === null ? call.name : `${call.type}.${call.name}`}\n    ${call.text}`;
    }));
  assert.deepEqual(unexported, [], `a page calls a name the wasm module does not export:\n  ${unexported.join('\n  ')}`);
});

test('a member of an exported class is held to the methods its impl exports', () => {
  const source = [
    'const peer = runtime.WasmPeer.start(config);',
    'runtime.WasmPeer.begin(config);',
    'runtime.Absent.start(config);',
  ].join('\n');
  assert.deepEqual(unexportedCalls(surface, source).map(call => `${call.type}.${call.name}`),
    ['WasmPeer.begin', 'Absent.start']);
});

test('the surface reads a free function under its js_name, or its Rust name when it has none', () => {
  const found = readSource([
    '#[wasm_bindgen(js_name = channelVault)]',
    'pub async fn channel_vault(namespace: String) -> Result<Vec<u8>, JsError> {',
    '    unimplemented!()',
    '}',
    '#[wasm_bindgen]',
    'pub fn prepare_close(id: u64) -> Result<String, JsValue> {',
    '    unimplemented!()',
    '}',
  ].join('\n'));
  assert.deepEqual([...found.module].sort(), ['channelVault', 'prepare_close']);
});

test('the surface reads a type and the methods its #[wasm_bindgen] impl makes callable', () => {
  const found = readSource([
    '#[wasm_bindgen]',
    'pub struct WasmPeer {',
    '    inner: Inner,',
    '}',
    '',
    '#[wasm_bindgen]',
    'impl WasmPeer {',
    '    /// A doc comment with a brace } in it does not close the impl.',
    '    pub fn start(&self, config: &str) { unimplemented!() }',
    '',
    '    #[wasm_bindgen(getter, js_name = deviceKey)]',
    '    pub fn device_key(&self) -> String { unimplemented!() }',
    '',
    '    #[wasm_bindgen(constructor)]',
    '    pub fn new() -> WasmPeer { unimplemented!() }',
    '',
    '    #[wasm_bindgen(skip)]',
    '    pub fn hidden(&self) {}',
    '',
    '    fn private(&self) {}',
    '}',
    '',
    '#[wasm_bindgen]',
    'pub enum Mode {',
    '    One,',
    '}',
  ].join('\n'));
  assert.deepEqual([...found.module].sort(), ['Mode', 'WasmPeer']);
  assert.deepEqual([...found.types.get('WasmPeer')].sort(), ['deviceKey', 'start']);
});

test('the surface reads through the attributes that follow a #[wasm_bindgen]', () => {
  const found = readSource([
    '#[wasm_bindgen]',
    '#[allow(',
    '    clippy::needless_pass_by_value,',
    '    reason = "wasm-bindgen receives JavaScript bytes only as an owned value"',
    ')]',
    'pub fn complete_asset(id: u64, rgba: Option<Vec<u8>>) -> Result<bool, JsValue> {',
    '    unimplemented!()',
    '}',
    '',
    '// A commented-out item is not an export.',
    '// #[wasm_bindgen]',
    '// pub fn removed() {}',
    '',
    '#[wasm_bindgen]',
    'extern "C" {',
    '    #[wasm_bindgen(js_name = imported)]',
    '    fn imported();',
    '}',
  ].join('\n'));
  assert.deepEqual([...found.module], ['complete_asset']);
});

test('a fake answers only names and members the runtime exports', () => {
  const honest = runtimeFake({parseLink: async () => '[]', WasmPeer: {start() {}}});
  assert.equal(typeof honest.parseLink, 'function');
  assert.equal(typeof honest.WasmPeer.start, 'function');

  assert.throws(() => runtimeFake({notAnExport: async () => '[]'}),
    /answers runtime\.notAnExport\(\), which the runtime does not export/);
  assert.throws(() => runtimeFake({WasmPeer: {notAMethod() {}}}),
    /answers runtime\.WasmPeer\.notAMethod\(\), which the runtime does not export/);
});

test('a fake keeps the test scaffolding the runtime knows nothing about', () => {
  let peer;
  const fake = runtimeFake({parseLink: async () => '[]'}, {
    calls: [],
    get peer() {
      return peer;
    },
  });
  fake.calls.push('one');
  peer = {ready: true};
  assert.deepEqual(fake.calls, ['one']);
  assert.deepEqual(fake.peer, {ready: true});
});

test('a commented-out call is not a call, and the calls that count keep their line', () => {
  const source = [
    '// runtime.gone() is commented out.',
    'const kept = runtime.nowhere?.(id);',
    '/* runtime.also_gone(); */',
    'const inline = () => runtime.take_call(id);',
  ].join('\n');
  assert.deepEqual(unexportedCalls(surface, source).map(call => [call.name, call.line]),
    [['nowhere', 2], ['take_call', 4]]);
  assert.deepEqual(withoutComments(source).split('\n').length, source.split('\n').length);
});
