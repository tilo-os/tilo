#!/usr/bin/env node
// Binary-level steps of the web runtime build. Usage:
//   wasm-tools.mjs strip-exports IN OUT REGEX   drop exports whose name matches
//   wasm-tools.mjs strip-bindgen-internals IN OUT  drop exports wasm-bindgen must own
//   wasm-tools.mjs add-section IN OUT NAME TEXT append a UTF-8 custom section
//   wasm-tools.mjs section IN NAME              print a custom section as text
//   wasm-tools.mjs exports IN                   print export names, one per line
//   wasm-tools.mjs imports-from IN MODULE       print imports from one module
//   wasm-tools.mjs import-names SIDE            symbols a side module needs from the core
//   wasm-tools.mjs linkable-symbols [--own CRATES] FILE...  defined non-local symbols of objects/rlibs
//   wasm-tools.mjs check-links SIDE CORE_EXPORTS  side module imports the core lacks
//   wasm-tools.mjs profile IN [TOP]             code size by crate and function
import {readFileSync, writeFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';

export function uleb(bytes, at) {
  let value = 0, shift = 0, byte;
  do {
    byte = bytes[at++];
    value += (byte & 0x7f) * 2 ** shift;
    shift += 7;
  } while (byte & 0x80);
  return [value, at];
}

export function encodeUleb(value) {
  const out = [];
  do {
    let byte = value % 128;
    value = Math.floor(value / 128);
    if (value) byte |= 0x80;
    out.push(byte);
  } while (value);
  return Uint8Array.from(out);
}

const utf8 = new TextDecoder();
const concat = parts => {
  const out = new Uint8Array(parts.reduce((n, part) => n + part.length, 0));
  let at = 0;
  for (const part of parts) {out.set(part, at); at += part.length;}
  return out;
};

/** Top-level sections: {id, start (of the id byte), body, end, name?}. */
export function sections(bytes) {
  if (utf8.decode(bytes.subarray(0, 4)) !== '\0asm') throw Error('not a wasm module');
  const out = [];
  let at = 8;
  while (at < bytes.length) {
    const start = at, id = bytes[at++];
    let size;
    [size, at] = uleb(bytes, at);
    const section = {id, start, body: at, end: at + size};
    if (id === 0) {
      const [length, name] = uleb(bytes, at);
      section.name = utf8.decode(bytes.subarray(name, name + length));
      section.payload = name + length;
    }
    out.push(section);
    at += size;
  }
  return out;
}

export function exportNames(bytes) {
  const section = sections(bytes).find(s => s.id === 7);
  if (!section) return [];
  let [count, at] = uleb(bytes, section.body);
  const names = [];
  while (count--) {
    let length;
    [length, at] = uleb(bytes, at);
    names.push(utf8.decode(bytes.subarray(at, at + length)));
    at += length + 1;
    [, at] = uleb(bytes, at);
  }
  return names;
}

/** `pattern` is a RegExp or a predicate over export names. */
export function stripExports(bytes, pattern) {
  const matches = typeof pattern === 'function' ? pattern : name => pattern.test(name);
  const parts = [bytes.subarray(0, 8)];
  let dropped = 0;
  for (const section of sections(bytes)) {
    if (section.id !== 7) {parts.push(bytes.subarray(section.start, section.end)); continue;}
    let [count, at] = uleb(bytes, section.body);
    const kept = [];
    while (count--) {
      const entry = at;
      let length;
      [length, at] = uleb(bytes, at);
      const name = utf8.decode(bytes.subarray(at, at + length));
      at += length + 1;
      [, at] = uleb(bytes, at);
      if (matches(name)) dropped++;
      else kept.push(bytes.subarray(entry, at));
    }
    const payload = concat([encodeUleb(kept.length), ...kept]);
    parts.push(Uint8Array.of(7), encodeUleb(payload.length), payload);
  }
  return {bytes: concat(parts), dropped};
}

const BINDGEN_CRATES = new Set(['wasm_bindgen', 'wasm_bindgen_futures', 'js_sys', 'web_sys']);
/**
 * Mangled exports wasm-bindgen must be able to remove or rewrite: items
 * defined in its crates and its `__wbindgen`/`__wbg_` internals. Its own
 * unmangled `__wbindgen_describe_*` exports stay, and so does a generic of
 * another crate that merely was instantiated in wasm_bindgen: apps link
 * against it.
 */
export const isBindgenInternal = name =>
  (name.startsWith('_R') || name.startsWith('_ZN')) &&
  (/__wbindgen|__wbg_|WasmDescribe|wasm_bindgen\d*8describe|describe_(closure|invoke|cast)/.test(name) || BINDGEN_CRATES.has(crateOf(name)));

export function customSection(bytes, name) {
  const section = sections(bytes).find(s => s.id === 0 && s.name === name);
  return section && bytes.subarray(section.payload, section.end);
}

/** Append a custom section, replacing any earlier one of that name. */
export function addSection(bytes, name, payload) {
  const parts = [bytes.subarray(0, 8)];
  for (const section of sections(bytes)) {
    if (!(section.id === 0 && section.name === name)) parts.push(bytes.subarray(section.start, section.end));
  }
  const encodedName = new TextEncoder().encode(name);
  const body = concat([encodeUleb(encodedName.length), encodedName, payload]);
  parts.push(Uint8Array.of(0), encodeUleb(body.length), body);
  return concat(parts);
}

/** Imports as {module, name, kind} (kind: 0 func, 1 table, 2 memory, 3 global). */
export function imports(bytes) {
  const section = sections(bytes).find(s => s.id === 2);
  if (!section) return [];
  let [count, at] = uleb(bytes, section.body);
  const out = [];
  const name = () => {
    let length;
    [length, at] = uleb(bytes, at);
    const text = utf8.decode(bytes.subarray(at, at + length));
    at += length;
    return text;
  };
  while (count--) {
    const module = name(), field = name(), kind = bytes[at++];
    if (kind === 0) [, at] = uleb(bytes, at);
    else if (kind === 1) {at++; const flags = bytes[at++]; [, at] = uleb(bytes, at); if (flags & 1) [, at] = uleb(bytes, at);}
    else if (kind === 2) {const flags = bytes[at++]; [, at] = uleb(bytes, at); if (flags & 1) [, at] = uleb(bytes, at);}
    else if (kind === 3) at += 2;
    else if (kind === 4) {at++; [, at] = uleb(bytes, at);}
    else throw Error(`unknown import kind ${kind}`);
    out.push({module, name: field, kind});
  }
  return out;
}

/** What the loader supplies itself rather than from the core's exports. */
const LOADER_PROVIDED = new Set(['memory', '__indirect_function_table', '__stack_pointer', '__memory_base', '__table_base']);

/**
 * Side-module imports the core cannot satisfy. GOT entries a module defines
 * itself are resolved from its own exports, which `--no-export-dynamic`
 * hides, so only env imports and GOT entries without a core export count.
 */
export function unresolvedImports(side, coreExports) {
  const core = new Set(coreExports);
  const missing = [], foreign = [];
  for (const entry of imports(side)) {
    if (entry.module === 'env') {
      if (!LOADER_PROVIDED.has(entry.name) && !core.has(entry.name)) missing.push(entry.name);
    } else if (entry.module === 'GOT.mem' || entry.module === 'GOT.func') {
      if (!core.has(entry.name)) missing.push(`${entry.module}.${entry.name}`);
    } else {
      foreign.push(`${entry.module}.${entry.name}`);
    }
  }
  return {missing, foreign};
}

/** Members of a GNU or BSD `ar` archive (an rlib): [{name, bytes}]. */
export function archiveMembers(bytes) {
  if (utf8.decode(bytes.subarray(0, 8)) !== '!<arch>\n') throw Error('not an archive');
  const members = [];
  let at = 8, longNames = '';
  while (at + 60 <= bytes.length) {
    let name = utf8.decode(bytes.subarray(at, at + 16)).trimEnd();
    const size = Number(utf8.decode(bytes.subarray(at + 48, at + 58)).trim());
    let body = at + 60, length = size;
    if (name === '//') longNames = utf8.decode(bytes.subarray(body, body + size));
    else if (/^\/\d+$/.test(name)) name = longNames.slice(Number(name.slice(1))).split(/\/?\n/)[0];
    else if (name.startsWith('#1/')) {
      const nameLength = Number(name.slice(3));
      name = utf8.decode(bytes.subarray(body, body + nameLength)).replace(/\0+$/, '');
      body += nameLength; length -= nameLength;
    } else name = name.replace(/\/$/, '');
    if (name && name !== '/' && name !== '//' && !name.startsWith('__.SYMDEF')) members.push({name, bytes: bytes.subarray(body, body + length)});
    at += 60 + size + (size % 2);
  }
  return members;
}

const SYMBOL_UNDEFINED = 0x10, SYMBOL_LOCAL = 0x2;
/**
 * Names of the functions and data an object file defines with non-local
 * binding: what another module may link against. From the `linking`
 * section's symbol table.
 */
export function definedSymbols(bytes) {
  const linking = sections(bytes).find(s => s.id === 0 && s.name === 'linking');
  if (!linking) return [];
  const names = [];
  let at = linking.payload;
  [, at] = uleb(bytes, at); // version
  const name = () => {
    let length;
    [length, at] = uleb(bytes, at);
    const text = utf8.decode(bytes.subarray(at, at + length));
    at += length;
    return text;
  };
  while (at < linking.end) {
    const type = bytes[at++];
    let size;
    [size, at] = uleb(bytes, at);
    const end = at + size;
    if (type === 8) { // WASM_SYMBOL_TABLE
      let count;
      [count, at] = uleb(bytes, at);
      while (count--) {
        const kind = bytes[at++];
        let flags;
        [flags, at] = uleb(bytes, at);
        const defined = !(flags & SYMBOL_UNDEFINED), exported = defined && !(flags & SYMBOL_LOCAL);
        if (kind === 1) { // data
          const symbol = name();
          if (defined) {[, at] = uleb(bytes, at); [, at] = uleb(bytes, at); [, at] = uleb(bytes, at);}
          if (exported) names.push(symbol);
        } else if (kind === 3) { // section
          [, at] = uleb(bytes, at);
        } else { // function, global, tag, table
          [, at] = uleb(bytes, at);
          if (defined || flags & 0x40) {
            const symbol = name();
            if (exported && kind === 0) names.push(symbol);
          }
        }
      }
    }
    at = end;
  }
  return names;
}

/** Defined non-local symbols of objects and archives (rlibs). */
export function linkableSymbols(files, read) {
  const names = new Set();
  for (const file of files) {
    const bytes = read(file);
    const objects = utf8.decode(bytes.subarray(0, 8)) === '!<arch>\n'
      ? archiveMembers(bytes).filter(member => member.name.endsWith('.o')).map(member => member.bytes)
      : [bytes];
    for (const object of objects) for (const symbol of definedSymbols(object)) names.add(symbol);
  }
  return [...names].sort();
}

/** Names from the `name` section: functions (kind 1) or data segments (kind 9), by index. */
function functionNames(bytes, wanted = 1) {
  const names = new Map();
  const section = sections(bytes).find(s => s.id === 0 && s.name === 'name');
  if (!section) return names;
  let at = section.payload;
  while (at < section.end) {
    const kind = bytes[at++];
    let size;
    [size, at] = uleb(bytes, at);
    const end = at + size;
    if (kind === wanted) {
      let count;
      [count, at] = uleb(bytes, at);
      while (count--) {
        let index, length;
        [index, at] = uleb(bytes, at);
        [length, at] = uleb(bytes, at);
        names.set(index, utf8.decode(bytes.subarray(at, at + length)));
        at += length;
      }
    }
    at = end;
  }
  return names;
}

const V0_CRATE = /C(?:s[0-9A-Za-z]*_)?(\d+)/;
/** The crate a (mangled) Rust symbol belongs to, best effort. */
export function crateOf(symbol) {
  if (symbol.startsWith('_ZN')) {
    const match = /^_ZN(\d+)/.exec(symbol);
    if (match) {
      const length = Number(match[1]), start = 3 + match[1].length;
      const name = symbol.slice(start, start + length);
      return name.startsWith('_$LT$') ? '<impl>' : name;
    }
  }
  if (symbol.startsWith('_R')) {
    const match = V0_CRATE.exec(symbol);
    if (match) {
      const start = match.index + match[0].length;
      let name = symbol.slice(start, start + Number(match[1]));
      if (name.startsWith('_')) name = name.slice(1);
      return name;
    }
  }
  // wasm-ld demangles the names of a side module: `<crate::a::B as ...>`, `crate::f`.
  const demangled = /^<*([a-z_][a-z0-9_]*)::/.exec(symbol);
  if (demangled) return demangled[1];
  return '<other>';
}

export function profile(bytes) {
  const all = sections(bytes);
  const importedFunctions = imports(bytes).filter(entry => entry.kind === 0).length;
  const names = functionNames(bytes);
  const functions = [];
  const code = all.find(s => s.id === 10);
  if (code) {
    let [count, at] = uleb(bytes, code.body);
    for (let index = importedFunctions; count--; index++) {
      const [size, body] = uleb(bytes, at);
      functions.push({name: names.get(index) ?? `func${index}`, bytes: body - at + size});
      at = body + size;
    }
  }
  const data = all.filter(s => s.id === 11).reduce((n, s) => n + s.end - s.start, 0);
  const custom = all.filter(s => s.id === 0).map(s => ({name: s.name, bytes: s.end - s.start}));
  // Data segments, named per symbol when linked with --no-merge-data-segments.
  const segments = [];
  const dataSection = all.find(s => s.id === 11);
  if (dataSection) {
    const segmentNames = functionNames(bytes, 9);
    let [count, at] = uleb(bytes, dataSection.body);
    for (let index = 0; index < count; index++) {
      const flags = bytes[at++];
      if (flags === 2) [, at] = uleb(bytes, at);
      if (flags !== 1) {while (bytes[at] !== 0x0b) at++; at++;}
      let size;
      [size, at] = uleb(bytes, at);
      segments.push({name: (segmentNames.get(index) ?? `segment${index}`).replace(/^\.(rodata|data|tdata|bss)\./, ''), bytes: size});
      at += size;
    }
  }
  const crates = new Map();
  const tally = (name, size, key) => {
    const crate = crateOf(name);
    const row = crates.get(crate) ?? {crate, bytes: 0, code: 0, data: 0, functions: 0};
    row.bytes += size; row[key] += size;
    if (key === 'code') row.functions++;
    crates.set(crate, row);
  };
  for (const fn of functions) tally(fn.name, fn.bytes, 'code');
  if (segments.length > 2) for (const segment of segments) tally(segment.name, segment.bytes, 'data');
  return {
    total: bytes.length,
    code: functions.reduce((n, fn) => n + fn.bytes, 0),
    data, custom,
    crates: [...crates.values()].sort((a, b) => b.bytes - a.bytes),
    functions: functions.sort((a, b) => b.bytes - a.bytes),
    segments: segments.sort((a, b) => b.bytes - a.bytes),
  };
}

function formatProfile(result, top) {
  const kib = n => `${(n / 1024).toFixed(1)} KiB`;
  const lines = [`total ${kib(result.total)}  code ${kib(result.code)}  data ${kib(result.data)}`];
  for (const section of result.custom) lines.push(`custom ${section.name}: ${kib(section.bytes)}`);
  lines.push('', 'by crate (code + data):');
  for (const row of result.crates.slice(0, top)) {
    lines.push(`${kib(row.bytes).padStart(12)}  code ${kib(row.code).padStart(11)}  data ${kib(row.data).padStart(11)}  ${String(row.functions).padStart(6)} fns  ${row.crate}`);
  }
  lines.push('', 'largest functions:');
  for (const fn of result.functions.slice(0, top)) lines.push(`${kib(fn.bytes).padStart(12)}  ${fn.name}`);
  if (result.segments.length > 2) {
    lines.push('', 'largest data:');
    for (const segment of result.segments.slice(0, top)) lines.push(`${kib(segment.bytes).padStart(12)}  ${segment.name}`);
  }
  return lines.join('\n');
}

function main([command, ...args]) {
  const read = path => new Uint8Array(readFileSync(path));
  switch (command) {
  case 'strip-exports': {
    const [input, output, pattern] = args;
    const {bytes, dropped} = stripExports(read(input), new RegExp(pattern));
    writeFileSync(output, bytes);
    console.error(`strip-exports: dropped ${dropped} exports matching /${pattern}/`);
    break;
  }
  case 'strip-bindgen-internals': {
    const [input, output] = args;
    const {bytes, dropped} = stripExports(read(input), isBindgenInternal);
    writeFileSync(output, bytes);
    console.error(`strip-bindgen-internals: dropped ${dropped} exports`);
    break;
  }
  case 'add-section': {
    const [input, output, name, text] = args;
    writeFileSync(output, addSection(read(input), name, new TextEncoder().encode(text)));
    break;
  }
  case 'section': {
    const payload = customSection(read(args[0]), args[1]);
    if (!payload) {console.error(`no custom section ${args[1]}`); process.exit(1);}
    process.stdout.write(utf8.decode(payload));
    break;
  }
  case 'exports':
    process.stdout.write(exportNames(read(args[0])).join('\n') + '\n');
    break;
  case 'linkable-symbols': {
    // --own CRATES: only items those crates define, not their instantiations
    // of other crates' generics.
    let own;
    if (args[0] === '--own') {own = new Set(args[1].split(/\s+/).filter(Boolean)); args = args.slice(2);}
    const symbols = linkableSymbols(args, read).filter(symbol => !own || own.has(crateOf(symbol)));
    process.stdout.write(symbols.join('\n') + '\n');
    break;
  }
  case 'import-names':
    // What a side module needs from the core, one name per line.
    for (const entry of imports(read(args[0]))) {
      if (['env', 'GOT.mem', 'GOT.func'].includes(entry.module) && !LOADER_PROVIDED.has(entry.name)) console.log(entry.name);
    }
    break;
  case 'imports-from':
    for (const entry of imports(read(args[0])).filter(entry => entry.module === args[1])) console.log(`${entry.module}.${entry.name}`);
    break;
  case 'check-links': {
    const exported = readFileSync(args[1], 'utf8').split('\n').filter(Boolean);
    const {missing, foreign} = unresolvedImports(read(args[0]), exported);
    for (const name of foreign) console.log(`foreign ${name}`);
    for (const name of missing) console.log(`missing ${name}`);
    process.exit(missing.length || foreign.length ? 1 : 0);
    break;
  }
  case 'profile':
    console.log(formatProfile(profile(read(args[0])), Number(args[1] ?? 40)));
    break;
  default:
    console.error('usage: wasm-tools.mjs strip-exports|add-section|section|exports|check-links|profile ...');
    process.exit(2);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main(process.argv.slice(2));
