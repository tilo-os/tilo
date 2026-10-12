// Review-owned ABI host, embedded in the opt-in bridge. No vendor JS is executed.
'use strict';
(() => {
  const limit = 512 * 1024 * 1024;
  const fail = () => { throw new Error('compiled speech operation failed'); };
  const configuration = JSON.parse(configurationJSON);
  let memory;
  const view = () => new DataView(memory.buffer);
  const range = (pointer, size) => {
    if (!Number.isSafeInteger(pointer) || !Number.isSafeInteger(size) || pointer < 0 || size < 0 ||
        pointer + size > memory.buffer.byteLength) fail('memory range');
  };
  const write32 = (pointer, value) => { range(pointer, 4); view().setUint32(pointer, value, true); };
  const unavailable = () => -52;
  const imports = {a: {
    a: () => fail('native exception'), e: () => fail('native abort'),
    d: unavailable, i: unavailable, p: unavailable, n: unavailable, o: unavailable,
    f: () => -8, j: () => 0,
    k: (count, size) => { write32(count, 0); write32(size, 0); return 0; },
    l: (clock, _precision, output) => {
      if (clock !== 0 && clock !== 1) fail('clock');
      range(output, 8);
      const ns = clock === 0 ? BigInt(Date.now()) * 1000000n : BigInt(Math.floor(workerClock() * 1e9));
      view().setBigUint64(output, ns, true); return 0;
    },
    m: wanted => {
      if (!Number.isInteger(wanted) || wanted < 0 || wanted > limit) return 0;
      const pages = Math.ceil(wanted / 65536) - memory.buffer.byteLength / 65536;
      try { if (pages > 0) memory.grow(pages); return 1; } catch { return 0; }
    },
    b: fd => fd >= 0 && fd <= 2 ? 0 : 8,
    g: () => 70,
    h: (fd, _vectors, _count, output) => { if (fd !== 0) fail('file read'); write32(output, 0); return 0; },
    c: (fd, vectors, count, output) => {
      if ((fd !== 1 && fd !== 2) || count < 0 || count > 16) fail('diagnostic vectors');
      range(vectors, count * 8);
      let total = 0;
      for (let index = 0; index < count; index++) {
        const pointer = view().getUint32(vectors + index * 8, true);
        const size = view().getUint32(vectors + index * 8 + 4, true);
        range(pointer, size); total += size;
        if (total > 65536) fail('diagnostic size');
      }
      write32(output, total); return 0;
    },
  }};
  const module = new WebAssembly.Module(moduleBytes);
  for (const item of WebAssembly.Module.imports(module)) {
    if (item.module !== 'a' || item.kind !== 'function' || !Object.hasOwn(imports.a, item.name)) fail('import');
  }
  const instance = new WebAssembly.Instance(module, imports);
  const engine = instance.exports;
  memory = engine.q;
  if (!(memory instanceof WebAssembly.Memory) || memory.buffer.byteLength > limit) fail('memory');
  engine.r();
  const allocate = bytes => {
    const pointer = engine.E(bytes.length);
    if (pointer <= 0) fail('allocation');
    range(pointer, bytes.length); new Uint8Array(memory.buffer, pointer, bytes.length).set(bytes);
    return pointer;
  };
  const model = allocate(new Uint8Array(modelBytes));
  if (engine.y(model, BigInt(modelBytes.byteLength)) < 0 || !(engine.z() & 2)) fail('speech model');
  // Keep model allocation alive: the public load contract does not transfer ownership.
  const pcm = allocate(new Uint8Array(30 * 16000 * 4));
  const output = allocate(new Uint8Array(65536));
  const encode = text => {
    const escaped = encodeURIComponent(text);
    const bytes = [];
    for (let index = 0; index < escaped.length;) {
      if (escaped[index] === '%') {
        bytes.push(parseInt(escaped.slice(index + 1, index + 3), 16));
        index += 3;
      } else {
        bytes.push(escaped.charCodeAt(index++));
      }
    }
    bytes.push(0);
    return Uint8Array.from(bytes);
  };
  const language = configuration.language === 'auto' ? 0 : allocate(encode(configuration.language));
  const prompt = configuration.vocabulary.length ? allocate(encode(configuration.vocabulary.join('\n'))) : 0;
  const decode = () => {
    const bytes = new Uint8Array(memory.buffer, output, 65536);
    const end = bytes.indexOf(0);
    if (end < 0) fail('output length');
    // V8 alone has no TextDecoder host; decoding via URI rejects malformed UTF-8.
    let encoded = '';
    for (const byte of bytes.subarray(0, end)) encoded += `%${byte.toString(16).padStart(2, '0')}`;
    return JSON.parse(decodeURIComponent(encoded));
  };
  const clearOutput = () => new Uint8Array(memory.buffer, output, 65536).fill(0);
  const join = (left, right) => left && right ? `${left} ${right}` : left || right;
  let stable = '';
  let retired = false;
  let accepted = 0;
  const silence = samples => samples.every(sample => sample === 0);
  globalThis.workerCall = (operation, bytes) => {
    if (retired) fail();
    const samples = new Float32Array(bytes);
    if (samples.length > 30 * 16000) fail();
    for (const sample of samples) if (!Number.isFinite(sample) || Math.abs(sample) > 1) fail();
    clearOutput();
    if (operation === 0) {
      accepted += samples.length;
      if (accepted > 30 * 16000) fail();
      new Float32Array(memory.buffer, pcm, samples.length).set(samples);
      if (engine.v(pcm, samples.length, language, prompt, output, 65536) < 0) fail();
      const result = decode();
      stable = join(stable, result.text);
      return join(stable, result.pending);
    }
    if (operation !== 1 || samples.length !== accepted) fail();
    retired = true;
    if (engine.w(output, 65536) < 0) fail();
    // Exact endpoint replaces every provisional word; no transcript rewrite.
    if (!samples.length || silence(samples)) return '';
    clearOutput();
    new Float32Array(memory.buffer, pcm, samples.length).set(samples);
    if (engine.x(pcm, samples.length, language, prompt, 1, output, 65536) < 0) fail();
    const result = decode();
    if (typeof result.text !== 'string') fail();
    return result.text;
  };
})();
