// Hand-encoded modules shaped like the web core (a non-PIE main module
// exporting memory, table, stack pointer and symbols) and a wasm-ld `-shared`
// side module, so the loader is tested without a Rust build.
const I32 = 0x7f, FUNCREF = 0x70;
const text = new TextEncoder();

function uleb(value) {
  const out = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value) byte |= 0x80;
    out.push(byte);
  } while (value);
  return out;
}
function sleb(value) {
  const out = [];
  for (;;) {
    const byte = value & 0x7f;
    value >>= 7;
    if ((value === 0 && !(byte & 0x40)) || (value === -1 && byte & 0x40)) {out.push(byte); return out;}
    out.push(byte | 0x80);
  }
}
const name = string => {const bytes = [...text.encode(string)]; return [...uleb(bytes.length), ...bytes];};
const vec = items => [...uleb(items.length), ...items.flat()];
const section = (id, bytes) => [id, ...uleb(bytes.length), ...bytes];
const custom = (title, payload) => section(0, [...name(title), ...payload]);
const module = sections => Uint8Array.from([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, ...sections.flat()]);
const func = (params, results) => [0x60, ...vec(params.map(p => [p])), ...vec(results.map(r => [r]))];
const body = (code, locals = []) => {const bytes = [...vec(locals), ...code, 0x0b]; return [...uleb(bytes.length), ...bytes];};

const op = {
  i32: value => [0x41, ...sleb(value)],
  local: index => [0x20, ...uleb(index)],
  tee: index => [0x22, ...uleb(index)],
  global: index => [0x23, ...uleb(index)],
  setGlobal: index => [0x24, ...uleb(index)],
  load: [0x28, 2, 0],
  add: [0x6a], sub: [0x6b], and: [0x71],
  call: index => [0x10, ...uleb(index)],
  callIndirect: type => [0x11, ...uleb(type), 0],
};

export const CORE_DATA_ADDRESS = 1024;
export const CORE_DATA_VALUE = 42;

/**
 * Exports: memory, __indirect_function_table (growable), __stack_pointer,
 * core_data (the address of a word holding 42), tilo_dl_alloc (a bump
 * allocator), core_add(a, b), call_slot(index) (calls table[index]() -> i32),
 * and the __wbindgen_start the glue calls.
 * `omit` leaves exports out, as a core linked without `dylink` would; an
 * `abi` of null leaves out the runtime ABI section.
 */
export function coreModule({abi = 'test-abi', omit = []} = {}) {
  const exports = [
    ['memory', 2, 0], ['__indirect_function_table', 1, 0], ['__stack_pointer', 3, 0], ['core_data', 3, 2],
    ['tilo_dl_alloc', 0, 0], ['core_add', 0, 1], ['call_slot', 0, 2], ['__wbindgen_start', 0, 3],
  ].filter(([title]) => !omit.includes(title));
  return module([
    section(1, vec([func([I32, I32], [I32]), func([I32], [I32]), func([], [I32]), func([], [])])),
    section(3, vec([[0], [0], [1], [3]])),
    section(4, vec([[FUNCREF, 0x00, ...uleb(1)]])),
    section(5, vec([[0x00, ...uleb(1)]])),
    section(6, vec([
      [I32, 1, ...op.i32(8192), 0x0b], // __stack_pointer
      [I32, 1, ...op.i32(2048), 0x0b], // allocator heap
      [I32, 0, ...op.i32(CORE_DATA_ADDRESS), 0x0b], // core_data
    ])),
    section(7, vec(exports.map(([title, kind, index]) => [...name(title), kind, ...uleb(index)]))),
    section(10, vec([
      // (heap + align - 1) & -align, then heap += size
      body([...op.global(1), ...op.local(1), ...op.add, ...op.i32(1), ...op.sub, ...op.i32(0), ...op.local(1), ...op.sub, ...op.and,
        ...op.tee(2), ...op.local(0), ...op.add, ...op.setGlobal(1), ...op.local(2)], [[1, I32]]),
      body([...op.local(0), ...op.local(1), ...op.add]),
      body([...op.local(0), ...op.callIndirect(2)]),
      body([]),
    ])),
    section(11, vec([[0, ...op.i32(CORE_DATA_ADDRESS), 0x0b, ...vec([CORE_DATA_VALUE, 0, 0, 0].map(b => [b]))]])),
    ...(abi === null ? [] : [custom('tilo.runtime_abi', [...text.encode(abi)])]),
  ]);
}

export const SIDE_OWN_VALUE = 99, SIDE_EXPORTED_VALUE = 55, SIDE_SLOT_RESULT = 7;

/**
 * A side module importing the core's memory, table, stack pointer, core_add
 * and GOT entries for core_data and core_add, plus a GOT entry for its own
 * side_data. Exports:
 *   tilo_app_register() -> core_add(*core_data, 1)
 *   read_own() -> its first data word (99), read_side_data() -> via GOT (55)
 *   got_func() -> core_add's table slot, own_slot() -> its table base
 * where table[own_slot()]() returns 7.
 */
export function sideModule({abi = 'test-abi', extraImport, memorySize = 8} = {}) {
  const imports = [
    [...name('env'), ...name('memory'), 2, 0x00, ...uleb(1)],
    [...name('env'), ...name('__indirect_function_table'), 1, FUNCREF, 0x00, ...uleb(0)],
    [...name('env'), ...name('__stack_pointer'), 3, I32, 1],
    [...name('env'), ...name('__memory_base'), 3, I32, 0], // global 1
    [...name('env'), ...name('__table_base'), 3, I32, 0], // global 2
    [...name('env'), ...name('core_add'), 0, 0], // function 0
    [...name('GOT.mem'), ...name('core_data'), 3, I32, 1], // global 3
    [...name('GOT.func'), ...name('core_add'), 3, I32, 1], // global 4
    [...name('GOT.mem'), ...name('side_data'), 3, I32, 1], // global 5
    ...(extraImport ? [[...name('env'), ...name(extraImport), 0, 1]] : []),
  ];
  const functions = extraImport ? 2 : 1;
  const own = index => index + functions;
  return module([
    custom('dylink.0', dylinkMemInfo(memorySize)),
    section(1, vec([func([I32, I32], [I32]), func([], [I32])])),
    section(2, vec(imports)),
    section(3, vec([[1], [1], [1], [1], [1], [1]])),
    section(6, vec([[I32, 0, ...op.i32(4), 0x0b]])), // global 6: side_data offset
    section(7, vec([
      [...name('tilo_app_register'), 0, ...uleb(own(1))],
      [...name('read_own'), 0, ...uleb(own(2))],
      [...name('got_func'), 0, ...uleb(own(3))],
      [...name('own_slot'), 0, ...uleb(own(4))],
      [...name('read_side_data'), 0, ...uleb(own(5))],
      [...name('side_data'), 3, ...uleb(6)],
    ])),
    section(9, vec([[0, ...op.global(2), 0x0b, ...vec([uleb(own(0))])]])),
    section(10, vec([
      body(op.i32(SIDE_SLOT_RESULT)),
      body([...op.global(3), ...op.load, ...op.i32(1), ...op.call(0)]),
      body([...op.global(1), ...op.load]),
      body(op.global(4)),
      body(op.global(2)),
      body([...op.global(5), ...op.load]),
    ])),
    section(11, vec([[0, ...op.global(1), 0x0b, ...vec([SIDE_OWN_VALUE, 0, 0, 0, SIDE_EXPORTED_VALUE, 0, 0, 0].map(b => [b]))]])),
    ...(abi === null ? [] : [custom('tilo.runtime_abi', [...text.encode(abi)])]),
  ]);
}

function dylinkMemInfo(memorySize) {
  const info = [...uleb(memorySize), 2, 1, 0]; // memory size, align 2^2, table size 1, align 2^0
  return [1, ...uleb(info.length), ...info];
}
