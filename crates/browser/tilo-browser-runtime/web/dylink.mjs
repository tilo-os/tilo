/**
 * Links app side modules into the web core (WEB_RUNTIME.md).
 *
 * The core is the main module: it exports its memory, function table, stack
 * pointer and every symbol. A side module (wasm-ld `-shared`, position
 * independent) imports those, places its data in memory the core's allocator
 * reserves, its functions in a slice of the core's table, and reaches the
 * core's data and function addresses through GOT globals filled in here.
 *
 * Symbol names do not reveal an ABI change, so a module is instantiated only
 * if its `tilo.runtime_abi` custom section names the core's runtime ABI id.
 */

export const ABI_SECTION = 'tilo.runtime_abi';
const decoder = new TextDecoder();

export class RuntimeAbiMismatch extends Error {
  constructor(name, expected, actual) {
    super(actual
      ? `${name} was built for runtime ABI ${actual}, but this core is ${expected}; it needs a build of ${name} for ${expected}`
      : `${name} carries no runtime ABI id (${ABI_SECTION}); it is not a side module of ${expected}`);
    this.name = 'RuntimeAbiMismatch';
    this.code = 'TILO_RUNTIME_ABI_MISMATCH';
    this.app = name;
    this.expected = expected;
    this.actual = actual;
  }
}

/** The runtime ABI id a compiled module carries, or undefined. */
export function runtimeAbi(module) {
  const [section] = WebAssembly.Module.customSections(module, ABI_SECTION);
  return section ? decoder.decode(section) : undefined;
}

/** Memory and table requirements from a side module's `dylink.0` section. */
export function readDylink(module) {
  const [section] = WebAssembly.Module.customSections(module, 'dylink.0');
  if (!section) throw Error('not a side module: no dylink.0 section');
  const bytes = new Uint8Array(section);
  let at = 0;
  const uleb = () => {
    let value = 0, shift = 0, byte;
    do {
      byte = bytes[at++];
      value += (byte & 0x7f) * 2 ** shift;
      shift += 7;
    } while (byte & 0x80);
    return value;
  };
  const info = {memorySize: 0, memoryAlign: 0, tableSize: 0, tableAlign: 0, needed: []};
  while (at < bytes.length) {
    const kind = bytes[at++], length = uleb(), end = at + length;
    if (kind === 1) { // WASM_DYLINK_MEM_INFO
      info.memorySize = uleb();
      info.memoryAlign = uleb();
      info.tableSize = uleb();
      info.tableAlign = uleb();
    } else if (kind === 2) { // WASM_DYLINK_NEEDED
      for (let count = uleb(); count--;) {
        const size = uleb();
        info.needed.push(decoder.decode(bytes.subarray(at, at + size)));
        at += size;
      }
    }
    at = end;
  }
  return info;
}

/** Compile what a resolver returned: a Module, a Response (streamed), or bytes. */
export async function compileModule(source) {
  source = await source;
  if (source instanceof WebAssembly.Module) return source;
  if (typeof Response !== 'undefined' && source instanceof Response) {
    if (!source.ok) throw Error(`side module HTTP ${source.status}`);
    // compileStreaming needs application/wasm; a plain static server may not say so.
    if (WebAssembly.compileStreaming && source.headers.get('content-type')?.startsWith('application/wasm')) {
      return WebAssembly.compileStreaming(source);
    }
    return WebAssembly.compile(await source.arrayBuffer());
  }
  if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) return WebAssembly.compile(source);
  throw TypeError('a side module is a WebAssembly.Module, a Response or bytes');
}

/**
 * Loads side modules against one instantiated core.
 *
 * `core` is the core's raw instance exports (what wasm-bindgen's init
 * returns when the glue was generated with --keep-lld-exports), `abi` the
 * core module's runtime ABI id.
 */
export class SideModuleLinker {
  constructor(core, abi) {
    for (const name of ['memory', '__indirect_function_table', '__stack_pointer', 'tilo_dl_alloc']) {
      if (!(name in core)) throw Error(`the core exports no ${name}: it was not linked as a dynamic-linking main module`);
    }
    if (!abi) throw Error(`the core carries no runtime ABI id (${ABI_SECTION})`);
    this.core = core;
    this.abi = abi;
    this.table = core.__indirect_function_table;
    // One table slot per function, shared by every module taking its address.
    this.slots = new Map();
  }

  /** Throws RuntimeAbiMismatch unless the module was built for this core. */
  check(module, name) {
    const actual = runtimeAbi(module);
    if (actual !== this.abi) throw new RuntimeAbiMismatch(name, this.abi, actual);
  }

  slot(fn) {
    let index = this.slots.get(fn);
    if (index === undefined) {
      index = this.table.grow(1);
      this.table.set(index, fn);
      this.slots.set(fn, index);
    }
    return index;
  }

  /**
   * Instantiate a side module and apply its relocations. Returns its exports.
   * Everything up to instantiation is synchronous, so concurrent links never
   * interleave their memory or table reservations.
   */
  async instantiate(module, name = 'side module') {
    this.check(module, name);
    const dylink = readDylink(module);
    if (dylink.needed.length) throw Error(`${name} needs other side modules (${dylink.needed.join(', ')}), which the loader does not support`);
    let memoryBase = 0;
    if (dylink.memorySize) {
      memoryBase = this.core.tilo_dl_alloc(dylink.memorySize, 2 ** dylink.memoryAlign);
      if (!memoryBase) throw Error(`${name}: the core could not reserve ${dylink.memorySize} bytes for its data`);
    }
    const tableAlign = 2 ** dylink.tableAlign;
    const tableBase = Math.ceil(this.table.length / tableAlign) * tableAlign;
    if (tableBase + dylink.tableSize > this.table.length) this.table.grow(tableBase + dylink.tableSize - this.table.length);

    const global = value => new WebAssembly.Global({value: 'i32', mutable: true}, value);
    const env = {
      memory: this.core.memory,
      __indirect_function_table: this.table,
      __stack_pointer: this.core.__stack_pointer,
      __memory_base: new WebAssembly.Global({value: 'i32', mutable: false}, memoryBase),
      __table_base: new WebAssembly.Global({value: 'i32', mutable: false}, tableBase),
    };
    const gotMem = {}, gotFunc = {}, ownSymbols = [], missing = [];
    for (const {module: from, name: symbol} of WebAssembly.Module.imports(module)) {
      const exported = this.core[symbol];
      if (from === 'env') {
        if (symbol in env) continue;
        if (typeof exported === 'function') env[symbol] = exported;
        else missing.push(symbol);
      } else if (from === 'GOT.mem') {
        gotMem[symbol] = global(0);
        if (exported instanceof WebAssembly.Global) gotMem[symbol].value = exported.value;
        else ownSymbols.push(['mem', symbol, gotMem[symbol]]);
      } else if (from === 'GOT.func') {
        gotFunc[symbol] = global(0);
        if (typeof exported === 'function') gotFunc[symbol].value = this.slot(exported);
        else ownSymbols.push(['func', symbol, gotFunc[symbol]]);
      } else {
        missing.push(`${from}.${symbol}`);
      }
    }
    if (missing.length) {
      throw Error(`${name} imports what the core does not export (${missing.length}): ${missing.slice(0, 8).join(', ')}${missing.length > 8 ? ', ...' : ''}`);
    }
    const instance = await WebAssembly.instantiate(module, {env, 'GOT.mem': gotMem, 'GOT.func': gotFunc});
    const exports = instance.exports;
    for (const [kind, symbol, entry] of ownSymbols) {
      const own = exports[symbol];
      if (kind === 'mem' && own instanceof WebAssembly.Global) entry.value = memoryBase + own.value;
      else if (kind === 'func' && typeof own === 'function') entry.value = this.slot(own);
      else throw Error(`${name}: unresolved GOT.${kind}.${symbol}`);
    }
    exports.__wasm_apply_data_relocs?.();
    exports.__wasm_call_ctors?.();
    return exports;
  }
}
