import assert from 'node:assert/strict';
// Inspect final bytes after bindgen/optimization; linker flags alone are insufficient.
export function inspectArtifact(bytes, {release = true} = {}) {
  if (release) assert.ok(bytes.length <= 1024 * 1024, 'separate initial 1 MiB release compute budget');
  const module = new WebAssembly.Module(bytes); // Reject malformed modules before interpreting sections.
  const imports = WebAssembly.Module.imports(module);
  assert.ok(imports.length <= (release ? 2 : 3) && imports.every(i => i.kind === 'function' &&
    i.module === './tilo_browser_compute_bg.js' && (/^(__wbg___wbindgen_throw_|__wbindgen_cast_)/.test(i.name) || (!release && i.name === '__wbindgen_init_externref_table'))),
  'image kernel imports only bindgen string/error glue');
  let pos = 8;
  const leb = () => {
    let value = 0, shift = 0, byte;
    do { byte = bytes[pos++]; value += (byte & 127) * 2 ** shift; shift += 7; } while (byte & 128);
    return value;
  };
  let memory;
  while (pos < bytes.length) {
    const section = bytes[pos++], length = leb(), end = pos + length;
    if (section === 5) {
      assert.equal(leb(), 1, 'one private memory');
      const flags = leb(), initial = leb(), maximum = flags & 1 ? leb() : null;
      memory = {flags, initial, maximum};
    }
    pos = end;
  }
  assert.ok(memory, 'artifact memory section');
  assert.equal(memory.flags, 1, 'private nonshared memory with maximum');
  assert.equal(memory.maximum, 4096, '256 MiB WASM ceiling');
  return memory;
}
