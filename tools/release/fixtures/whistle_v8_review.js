// Review-owned host glue; the publisher's JavaScript is read as an API mapping,
// never executed. Only authenticated public fixture buffers enter this context.
'use strict';
(() => {
  const emit = (kind, value) => reviewPrint(`WHISTLE_V8_${kind} ${JSON.stringify(value)}`);
  const limit = 512 * 1024 * 1024;
  const fail = message => { throw new Error(message); };
  if (!['stream', 'endpoint', 'lifecycle'].includes(reviewMode)) fail('review mode');
  if (!['en', 'de', 'fr', 'es', 'it', 'nl', 'pl'].includes(reviewLanguage)) fail('language');
  if (reviewPrompt && (reviewLanguage !== 'de' || reviewPrompt !== 'Hongkong\nKowloon')) fail('public cue');
  const audio = new Float32Array(audioBytes);
  if (!audio.length || audio.length > 30 * 16000) fail('audio length');
  for (const sample of audio) if (!Number.isFinite(sample) || Math.abs(sample) > 1) fail('sample');
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
      const ns = clock === 0 ? BigInt(Date.now()) * 1000000n : BigInt(Math.floor(reviewClock() * 1e9));
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
  const beginning = reviewClock();
  const module = new WebAssembly.Module(moduleBytes);
  const compiled = reviewClock();
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
  const language = allocate(encode(reviewLanguage));
  const prompt = reviewPrompt ? allocate(encode(reviewPrompt)) : 0;
  emit('LOAD', {compile_seconds: compiled - beginning, initialize_load_seconds: reviewClock() - compiled,
    v8_backend: 'existing public single-threaded platform', resident: true});
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
  const runStream = (samples, session, cancelled = false) => {
    const start = reviewClock();
    let offset = 0;
    let planned = 32000;
    let stable = '';
    while (offset < samples.length) {
      planned = Math.min(planned, samples.length);
      reviewPause(Math.max(0, planned / 16000 - (reviewClock() - start)));
      const end = Math.min(samples.length, Math.max(planned, Math.floor((reviewClock() - start) * 16000)));
      clearOutput();
      new Float32Array(memory.buffer, pcm, end - offset).set(samples.subarray(offset, end));
      const decoding = reviewClock();
      if (engine.v(pcm, end - offset, language, prompt, output, 65536) < 0) fail('stream process');
      const result = decode();
      stable = join(stable, result.text);
      // Native two-pass agreement remains provisional for OS field insertion.
      emit('REPLY', {session, kind: 'draft', wall_seconds: reviewClock() - start,
        decode_seconds: reviewClock() - decoding, result,
        policy: {provisional: join(stable, result.pending), committed: ''}});
      offset = end; planned = offset + 16000;
    }
    clearOutput();
    const stopping = reviewClock();
    if (engine.w(output, 65536) < 0) fail('stream stop');
    const stopped = decode();
    stable = join(stable, stopped.text);
    if (cancelled) {
      emit('REPLY', {session, kind: 'canceled', wall_seconds: reviewClock() - start,
        discarded_result: stopped, discarded_stream_text: stable,
        policy: {provisional: '', committed: ''}});
      return;
    }
    if (reviewMode !== 'stream') {
      // Public needle_transcribe: exact complete audio, selected language and
      // the same dictionary bias. It replaces the provisional hypothesis;
      // no dictionary substitution or reference-aware text cleanup follows it.
      new Float32Array(memory.buffer, pcm, samples.length).set(samples);
      clearOutput();
      const endpoint = reviewClock();
      if (engine.x(pcm, samples.length, language, prompt, 1, output, 65536) < 0) fail('endpoint transcribe');
      const selected = decode();
      const selectedSeconds = reviewClock() - endpoint;
      emit('ENDPOINT', {session, raw_stream_text: stable, raw_selected: selected,
        selected_language: reviewLanguage, selected_decode_seconds: selectedSeconds,
        selected_wall_seconds: reviewClock() - start, committed: selected.text});
      emit('REPLY', {session, kind: 'done', wall_seconds: reviewClock() - start,
        result: stopped, policy: {provisional: '', committed: selected.text}});
      clearOutput();
      const automatic = reviewClock();
      if (engine.x(pcm, samples.length, 0, prompt, 1, output, 65536) < 0) fail('language control');
      const detected = decode();
      emit('LANGUAGE_CONTROL', {session, raw_auto: detected,
        auto_decode_seconds: reviewClock() - automatic});
      clearOutput();
      const untimed = reviewClock();
      if (engine.x(pcm, samples.length, language, prompt, 0, output, 65536) < 0) fail('timestamp control');
      emit('TIMESTAMP_CONTROL', {session, raw_without_timestamps: decode(),
        decode_seconds: reviewClock() - untimed});
    } else {
      emit('REPLY', {session, kind: 'done', wall_seconds: reviewClock() - start,
        decode_seconds: reviewClock() - stopping, result: stopped,
        policy: {provisional: '', committed: stable}});
    }
  };
  if (reviewMode === 'lifecycle') {
    runStream(audio, 0);
    runStream(new Float32Array(3 * 16000), 1);
    runStream(audio.subarray(0, 3 * 16000), 2, true);
    runStream(audio, 3);
  } else {
    runStream(audio, 0);
  }
})();
