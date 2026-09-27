import test from 'node:test';
import assert from 'node:assert/strict';
import {RuntimeAbiMismatch, SideModuleLinker, compileModule, readDylink, runtimeAbi} from './dylink.mjs';
import {CORE_DATA_VALUE, SIDE_EXPORTED_VALUE, SIDE_OWN_VALUE, SIDE_SLOT_RESULT, coreModule, sideModule} from './testing/wasm-modules.mjs';

async function core(options) {
  const module = await WebAssembly.compile(coreModule(options));
  const {exports} = await WebAssembly.instantiate(module);
  return {exports, linker: new SideModuleLinker(exports, runtimeAbi(module))};
}

test('a side module reaches the core through env imports and GOT entries and owns its data and table slice', async () => {
  const {exports: main, linker} = await core();
  const side = await compileModule(sideModule());
  assert.deepEqual(readDylink(side), {memorySize: 8, memoryAlign: 2, tableSize: 1, tableAlign: 0, needed: []});
  const tableBefore = main.__indirect_function_table.length;
  const app = await linker.instantiate(side, 'so.tilo.test');
  // core_add(*core_data, 1): a core function called directly, core data through GOT.mem.
  assert.equal(app.tilo_app_register(), CORE_DATA_VALUE + 1);
  // Its data segment sits in memory the core's allocator reserved.
  assert.equal(app.read_own(), SIDE_OWN_VALUE);
  assert.equal(app.read_side_data(), SIDE_EXPORTED_VALUE, 'GOT.mem of a symbol it defines is relocated by its memory base');
  // Its element segment fills its own slice of the core's table, callable from the core.
  assert(app.own_slot() >= tableBefore);
  assert.equal(main.call_slot(app.own_slot()), SIDE_SLOT_RESULT);
  // GOT.func gives the core function one shared slot.
  assert.equal(main.__indirect_function_table.get(app.got_func()), main.core_add);
});

test('each linked module gets its own memory and table ranges; shared function slots are reused', async () => {
  const {exports: main, linker} = await core();
  const [first, second] = await Promise.all([
    compileModule(sideModule()).then(module => linker.instantiate(module, 'first')),
    compileModule(sideModule()).then(module => linker.instantiate(module, 'second')),
  ]);
  assert.notEqual(first.own_slot(), second.own_slot());
  assert.equal(main.call_slot(first.own_slot()), SIDE_SLOT_RESULT);
  assert.equal(main.call_slot(second.own_slot()), SIDE_SLOT_RESULT);
  assert.equal(first.read_own(), SIDE_OWN_VALUE);
  assert.equal(second.read_side_data(), SIDE_EXPORTED_VALUE);
  assert.equal(first.got_func(), second.got_func());
});

test('a module built for another runtime ABI is refused before it is instantiated', async () => {
  const {exports: main, linker} = await core({abi: 'tilo-runtime/2026.07+wasm.aaaa'});
  const tableBefore = main.__indirect_function_table.length;
  const module = await compileModule(sideModule({abi: 'tilo-runtime/2026.07+wasm.bbbb'}));
  await assert.rejects(linker.instantiate(module, 'so.tilo.ebook'), error => {
    assert(error instanceof RuntimeAbiMismatch);
    assert.equal(error.code, 'TILO_RUNTIME_ABI_MISMATCH');
    assert.equal(error.expected, 'tilo-runtime/2026.07+wasm.aaaa');
    assert.equal(error.actual, 'tilo-runtime/2026.07+wasm.bbbb');
    assert.match(error.message, /so\.tilo\.ebook was built for runtime ABI tilo-runtime\/2026\.07\+wasm\.bbbb, but this core is tilo-runtime\/2026\.07\+wasm\.aaaa/);
    return true;
  });
  assert.equal(main.__indirect_function_table.length, tableBefore, 'nothing was reserved for it');
  const unmarked = await compileModule(sideModule({abi: null}));
  await assert.rejects(linker.instantiate(unmarked, 'bare'), error => error.code === 'TILO_RUNTIME_ABI_MISMATCH' && error.actual === undefined && /carries no runtime ABI id/.test(error.message));
});

test('an import the core does not export names the symbol instead of failing inside instantiation', async () => {
  const {linker} = await core();
  const module = await compileModule(sideModule({extraImport: '_RNvCs1_7missing4gone'}));
  await assert.rejects(linker.instantiate(module, 'so.tilo.test'), /imports what the core does not export \(1\): _RNvCs1_7missing4gone/);
});

test('a core linked without dylink exports cannot link side modules', async () => {
  const module = await WebAssembly.compile(coreModule({omit: ['tilo_dl_alloc']}));
  const {exports} = await WebAssembly.instantiate(module);
  assert.throws(() => new SideModuleLinker(exports, runtimeAbi(module)), /exports no tilo_dl_alloc/);
  const unmarked = await WebAssembly.compile(coreModule({abi: null}));
  const unmarkedExports = (await WebAssembly.instantiate(unmarked)).exports;
  assert.throws(() => new SideModuleLinker(unmarkedExports, runtimeAbi(unmarked)), /carries no runtime ABI id/);
});

test('modules compile from bytes, a Response or an already compiled Module', async () => {
  const bytes = sideModule();
  const fromBytes = await compileModule(bytes);
  assert.equal(runtimeAbi(fromBytes), 'test-abi');
  assert.equal(await compileModule(fromBytes), fromBytes);
  assert.equal(runtimeAbi(await compileModule(new Response(bytes, {headers: {'content-type': 'application/wasm'}}))), 'test-abi');
  assert.equal(runtimeAbi(await compileModule(Promise.resolve(new Response(bytes)))), 'test-abi');
  await assert.rejects(compileModule(new Response('gone', {status: 404})), /side module HTTP 404/);
  await assert.rejects(compileModule({}), TypeError);
});
