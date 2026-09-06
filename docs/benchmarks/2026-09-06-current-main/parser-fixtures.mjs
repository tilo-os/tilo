import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { parse } from './parse.mjs';
const footer = '\nbenchmark adapter: name=llvmpipe (LLVM 22.1.8, 128 bits) backend=Vulkan device_type=Cpu driver=llvmpipe\nMaximum resident set size (kbytes): 400000\nElapsed (wall clock) time (h:mm:ss or m:ss): 0:50.00\n';
const shell = 'main DRM render/readback 3840x2160: samples=1000 total_us=40000999 mean=40000us p99=80000us min=10000us max=120000us\nmain XRGB destination checksum: final=0x123\nmain bridge limitation: no bump settlement/recovery telemetry\n' + footer;
const ui = '=== Dashboard bench (1000 events, canvas 3840×2160) ===\nredrawn events: 1000\nskipped events: 0\nTOTAL/event 80000 90000 100000\nbenchmark timing: samples=1000 redraws=1000 total_us=80000999 p99_rounded=100000us max=120000us cpu_target_readback_bytes=0 overflowed_submissions=0\nbenchmark validity: complete\n' + footer;
function parsed(workload, log, overrides={}) {
 const bytes=Buffer.from(log);
 return parse({name:'fixture',workload,revision:'main'}, {output_truncated:false,signal:null,spawn_error:null,exit_code:0,log_bytes:bytes.length,log_sha256:createHash('sha256').update(bytes).digest('hex'),...overrides},bytes);
}
assert.equal(parsed('shell',shell).complete,true);
assert.equal(parsed('shell',shell).mean_us,40000.999);
assert.equal(parsed('shell',shell).overflow_verified,false);
assert.equal(parsed('shell',shell.replaceAll('samples=1000','samples=999')).complete,false);
assert.equal(parsed('shell',shell,{exit_code:124}).complete,false);
assert.equal(parsed('shell',shell,{output_truncated:true}).complete,false);
assert.equal(parsed('shell',shell,{log_sha256:'bad'}).complete,false);
assert.equal(parsed('shell',shell+shell).complete,false);
assert.equal(parsed('ui',ui).complete,true);
assert.equal(parsed('ui',ui).mean_us,80000.999);
assert.equal(parsed('ui',ui.replace('overflowed_submissions=0','overflowed_submissions=1')).complete,false);
assert.equal(parsed('ui',ui.replace('cpu_target_readback_bytes=0','cpu_target_readback_bytes=33177600')).complete,false);
assert.equal(parsed('ui',ui.replace('redrawn events: 1000','redrawn events: 999')).complete,false);
assert.equal(parsed('ui',ui,{exit_code:134}).complete,false);
console.log('14 parser assertions passed; synthetic fixtures are not benchmark evidence.');
