/**
 * What the wasm module offers the page, and the one check that keeps the page
 * and its tests honest about it.
 *
 * The page reaches the runtime as `import * as runtime from
 * './pkg/tilo_browser_runtime.js'`, so every `runtime.name(...)` it writes is a
 * promise that the Rust crate exports `name`. Nothing sees across that
 * boundary: the `pkg/*.d.ts` exists only after a build, and a test that fakes
 * the runtime answers under the same invented name, so a page and its test can
 * agree on a name the module never had. The call then throws only in a browser,
 * in the build that reached a person.
 *
 * So the crate's Rust answers the question instead: its `#[wasm_bindgen]` items
 * are the only names a page may call and a fake may carry, and
 * `runtime-surface.test.mjs` holds every page module and every fake to them.
 */
import {readFileSync, readdirSync} from 'node:fs';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';

const web = fileURLToPath(new URL('.', import.meta.url));
const crate = join(web, '..');

/**
 * The one attribute this reads: `#[wasm_bindgen]`, and the arguments that
 * change the name a `#[wasm_bindgen(js_name = …)]` binds.
 */
function wasmBindgenAttribute(text) {
  return /^\s*#\[wasm_bindgen\b([^\]]*)\]/.exec(text);
}

/**
 * The attribute that opens at `lines[start]`, joined across the lines it spans
 * (`#[allow(\n  reason = "…"\n)]`), and the line just past it.
 */
function attributeAt(lines, start) {
  let text = '';
  for (let line = start; line < lines.length; line += 1) {
    text += line === start ? lines[line] : `\n${lines[line]}`;
    if (attributeClosed(text)) return {text, next: line + 1};
  }
  return {text, next: lines.length};
}

/** Whether `text` holds a whole `#[…]`, its brackets balanced outside quotes. */
function attributeClosed(text) {
  let depth = 0;
  let quoted = false;
  for (let index = 0; index < text.length; index += 1) {
    const ch = text[index];
    if (quoted) {
      if (ch === '\\') index += 1;
      else if (ch === '"') quoted = false;
      continue;
    }
    if (ch === '"') quoted = true;
    else if (ch === '[') depth += 1;
    else if (ch === ']') {
      depth -= 1;
      if (depth === 0) return true;
    }
  }
  return false;
}

/** The first line at or after `start` that is an item, not an attribute, comment or blank. */
function nextItem(lines, start) {
  let line = start;
  while (line < lines.length) {
    const trimmed = lines[line].trim();
    if (trimmed === '' || trimmed.startsWith('//')) {
      line += 1;
      continue;
    }
    if (trimmed.startsWith('#[')) {
      line = attributeAt(lines, line).next;
      continue;
    }
    return line;
  }
  return lines.length;
}

/** The line just past the block that opens at or after `from`. */
function blockEnd(lines, from) {
  let depth = 0;
  let opened = false;
  for (let line = from; line < lines.length; line += 1) {
    const text = lines[line];
    let quoted = false;
    for (let index = 0; index < text.length; index += 1) {
      const ch = text[index];
      if (quoted) {
        if (ch === '\\') index += 1;
        else if (ch === '"') quoted = false;
        continue;
      }
      // A line comment ends the line; a brace inside it nests nothing.
      if (ch === '/' && text[index + 1] === '/') break;
      if (ch === '"') quoted = true;
      else if (ch === '{') {
        depth += 1;
        opened = true;
      } else if (ch === '}') {
        depth -= 1;
        if (opened && depth === 0) return line + 1;
      }
    }
  }
  return lines.length;
}

/** `pub fn name`, `pub struct Name`, `pub const NAME`, … as one item. */
const ITEM = /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe|extern)\s+)*(fn|struct|enum|static|const)\s+([A-Za-z_][A-Za-z0-9_]*)/;

/** The name a `#[wasm_bindgen(…)]` binds, or null to take the Rust one. */
function boundName(attributeText) {
  return /js_name\s*=\s*([A-Za-z_][A-Za-z0-9_]*)/.exec(attributeText)?.[1] ?? null;
}

/**
 * The names one Rust source exports, and the methods of each exported type.
 *
 * `module` holds what `runtime.name` may be: a free function, a struct, an enum.
 * `types` holds `runtime.Type.method` for a struct whose `#[wasm_bindgen] impl`
 * makes its public methods callable on the exported class.
 */
export function readSource(text) {
  const lines = text.split('\n');
  const found = {module: new Set(), types: new Map()};
  let line = 0;
  while (line < lines.length) {
    if (!wasmBindgenAttribute(lines[line])) {
      line += 1;
      continue;
    }
    const attribute = attributeAt(lines, line);
    const itemLine = nextItem(lines, attribute.next);
    if (itemLine >= lines.length) break;
    const item = lines[itemLine];
    line = itemLine + 1;

    if (/^\s*(?:unsafe\s+)?impl\b/.test(item)) {
      const target = /impl\s*(?:<[^>]*>\s*)?(?:[A-Za-z_][\w:<>,\s]*?\s+for\s+)?([A-Za-z_][A-Za-z0-9_]*)/.exec(item);
      const end = blockEnd(lines, itemLine);
      if (target) collectMethods(lines, itemLine + 1, end, target[1], found);
      line = end;
      continue;
    }
    // `#[wasm_bindgen] extern "C" { … }` imports JavaScript; it exports nothing.
    if (/^\s*(?:pub\s+)?(?:unsafe\s+)?extern\b/.test(item)) {
      line = blockEnd(lines, itemLine);
      continue;
    }
    const decl = ITEM.exec(item);
    if (decl) found.module.add(boundName(attribute.text) ?? decl[2]);
  }
  return found;
}

/** The public methods of `#[wasm_bindgen] impl Type`, a constructor aside. */
function collectMethods(lines, from, end, type, found) {
  const methods = found.types.get(type) ?? new Set();
  found.types.set(type, methods);
  let line = from;
  while (line < end) {
    let attributes = '';
    while (line < end && lines[line].trim().startsWith('#[')) {
      const attribute = attributeAt(lines, line);
      attributes += attribute.text;
      line = attribute.next;
    }
    // `#[wasm_bindgen(skip)]` keeps a public method off the class.
    if (/^\s*#\[wasm_bindgen\b/.test(attributes) && /\bskip\b/.test(attributes)) {
      line = blockEnd(lines, nextItem(lines, line));
      continue;
    }
    const decl = /^\s*pub\s+(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(lines[line] ?? '');
    if (!decl) {
      line += 1;
      continue;
    }
    // `#[wasm_bindgen(constructor)]` builds the class; it names no member.
    if (!/\bconstructor\b/.test(attributes)) methods.add(boundName(attributes) ?? decl[1]);
    line = blockEnd(lines, line);
  }
}

/** Every `.rs` below `dir`. */
function rustSources(dir) {
  return readdirSync(dir, {recursive: true, withFileTypes: true})
    .filter(entry => entry.isFile() && entry.name.endsWith('.rs'))
    // `parentPath` replaced `path` on a directory entry; CI's node may be either.
    .map(entry => join(entry.parentPath ?? entry.path, entry.name));
}

/**
 * The runtime's export surface, read from the crate's Rust.
 *
 * `module` is what `runtime.name` may be; `types` is each exported type's public
 * methods, for `runtime.Type.method`.
 */
export function runtimeSurface({rustDir = join(crate, 'src')} = {}) {
  const module = new Set();
  const types = new Map();
  for (const file of rustSources(rustDir)) {
    const found = readSource(readFileSync(file, 'utf8'));
    for (const name of found.module) module.add(name);
    for (const [type, methods] of found.types) {
      const known = types.get(type) ?? new Set();
      for (const method of methods) known.add(method);
      types.set(type, known);
    }
  }
  return {module, types};
}

/**
 * `source` without its comments, so a commented-out call is not a call. Every
 * line break and every other character keeps its place, so a call that is found
 * is still reported at the line it is written on.
 */
export function withoutComments(source) {
  const out = [...source];
  const blank = at => {
    if (out[at] !== '\n') out[at] = ' ';
  };
  let index = 0;
  while (index < source.length) {
    const ch = source[index];
    const next = source[index + 1];
    if (ch === '/' && next === '/') {
      while (index < source.length && source[index] !== '\n') blank(index++);
      continue;
    }
    if (ch === '/' && next === '*') {
      const close = source.indexOf('*/', index + 2);
      const end = close === -1 ? source.length : close + 2;
      while (index < end) blank(index++);
      continue;
    }
    if (ch === '"' || ch === "'" || ch === '`') {
      index += 1;
      while (index < source.length) {
        const inner = source[index];
        index += 1;
        if (inner === '\\') index += 1;
        else if (inner === ch) break;
      }
      continue;
    }
    index += 1;
  }
  return out.join('');
}

/**
 * The `runtime.name(` and `runtime.name?.(` calls in one module, as
 * `{name, type, line, text}`, and `runtime.Type.method(…)` as `{type, name}`,
 * where `type` is null for a call on the module itself. Line numbers count the
 * source as written.
 */
export function runtimeCalls(source) {
  const lines = source.split('\n');
  const scanned = withoutComments(source).split('\n');
  const calls = [];
  scanned.forEach((text, index) => {
    // The member of an exported class the page calls, `runtime.WasmPeer.start(…)`.
    for (const match of text.matchAll(/\bruntime\s*\.\s*([A-Za-z_$][\w$]*)\s*\.\s*([A-Za-z_$][\w$]*)\s*(?:\?\.)?\s*\(/g)) {
      calls.push({type: match[1], name: match[2], line: index + 1, text: lines[index].trim()});
    }
    for (const match of text.matchAll(/\bruntime\s*\.\s*([A-Za-z_$][\w$]*)\s*(?:\?\.)?\s*\(/g)) {
      calls.push({type: null, name: match[1], line: index + 1, text: lines[index].trim()});
    }
  });
  return calls;
}

/** The runtime's JavaScript: its page modules, the tests aside. */
export function pageModules({webDir = web} = {}) {
  return readdirSync(webDir)
    .filter(name => name.endsWith('.mjs') && !name.endsWith('.test.mjs'))
    .map(name => ({name, source: readFileSync(join(webDir, name), 'utf8')}));
}

/**
 * The calls in `source` naming something the module does not export, each as
 * `{type, name, line, text}`.
 */
export function unexportedCalls(surface, source) {
  return runtimeCalls(source).filter(call => {
    if (!surface.module.has(call.type ?? call.name)) return true;
    return call.type === null ? false : !(surface.types.get(call.type)?.has(call.name) ?? false);
  });
}

/**
 * A stand-in for the runtime, held to the surface the real module offers.
 *
 * `surface` is what the fake answers: every member must be an export, and a
 * member the module exports as a type may answer only that type's methods.
 * `extra` is the test's own scaffolding (recorded calls, a live peer, a status
 * the test rewrites); the module knows nothing about it, so it is left alone.
 */
export function runtimeFake(surface, extra = {}, {where = 'the runtime fake', real = null} = {}) {
  const exported = real ?? (defaultSurface ??= runtimeSurface());
  for (const [name, value] of Object.entries(surface)) {
    if (!exported.module.has(name)) {
      throw new Error(`${where} answers runtime.${name}(), which the runtime does not export`);
    }
    const methods = exported.types.get(name);
    if (!methods || value === null || typeof value !== 'object') continue;
    for (const member of Object.keys(value)) {
      if (!methods.has(member)) {
        throw new Error(`${where} answers runtime.${name}.${member}(), which the runtime does not export`);
      }
    }
  }
  return Object.defineProperties({...surface}, Object.getOwnPropertyDescriptors(extra));
}

/** The crate's surface, read once: a suite builds many fakes. */
let defaultSurface;
