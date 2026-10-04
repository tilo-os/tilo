import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,copyFileSync,readdirSync} from 'node:fs';
import {resolve,join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {serve,launch} from '../../../../tools/web/chrome.mjs';
const root=resolve(fileURLToPath(new URL('../../../..',import.meta.url)));
const output=resolve(process.argv[2]??join(root,'target/browser-compute'));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
// Read the actual post-bindgen WASM memory section, not build-script intent.
const wasm=readFileSync(join(output,'pkg/compute/tilo_browser_compute_bg.wasm'));
assert.ok(wasm.length<=1024*1024,'separate 1 MiB compute module budget');
const imports=WebAssembly.Module.imports(new WebAssembly.Module(wasm));
assert.equal(imports.length,2);assert.ok(imports.every(i=>i.kind==='function'&&i.module==='./tilo_browser_compute_bg.js'&&/^(__wbg___wbindgen_throw_|__wbindgen_cast_)/.test(i.name)),'image kernel imports only bindgen string/error glue');
let pos=8;const leb=()=>{let n=0,s=0,b;do{b=wasm[pos++];n+=(b&127)*2**s;s+=7;}while(b&128);return n;};let memory;
while(pos<wasm.length){const section=wasm[pos++],length=leb(),end=pos+length;if(section===5){assert.equal(leb(),1);const flags=leb(),initial=leb(),maximum=(flags&1)?leb():null;memory={flags,initial,maximum};}pos=end;}
assert.ok(memory,'artifact memory section');assert.equal(memory.flags,1,'private memory with maximum');assert.equal(memory.maximum,4096);
copyFileSync(new URL('./browser-page.mjs',import.meta.url),join(output,'browser-page.mjs'));
writeFileSync(join(output,'index.html'),'<!doctype html><title>Private compute qualification</title>');
writeFileSync(join(output,'memory-fixture.mjs'),`import init from './pkg/compute/tilo_browser_compute.js';const wasm=await init();let growthRefused=false;try{wasm.memory.grow(4097);}catch(e){growthRefused=e instanceof RangeError;}postMessage({nonshared:wasm.memory.buffer instanceof ArrayBuffer,growthRefused,initialBytes:wasm.memory.buffer.byteLength});`);
writeFileSync(join(output,'hang-fixture.mjs'),'onmessage=()=>{while(true){}};');
writeFileSync(join(output,'trap-fixture.mjs'),'const instance=new WebAssembly.Instance(new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0,1,4,1,96,0,0,3,2,1,0,7,8,1,4,116,114,97,112,0,0,10,5,1,3,0,0,11])));onmessage=()=>instance.exports.trap();');
const server=await serve(output);let browser;
try{
  browser=await launch();const tab=await browser.tab();await tab.navigate(server.url);await tab.waitFor('document.readyState === "complete"');
  const result=await tab.evaluate("import('./browser-page.mjs').then(m=>m.qualify())");
  assert.equal(result.workerResults,4+result.work.iterations); // 3 formats + after-cancel + actual workload
  const sources={};
  function collect(dir){for(const e of readdirSync(dir,{withFileTypes:true})){const path=join(dir,e.name);if(e.isDirectory())collect(path);else sources[path.slice(root.length+1)]=sha(readFileSync(path));}}
  collect(join(root,'crates/browser/tilo-browser-compute'));
  const assets={};
  for(const name of ['compute-worker.mjs','compute-domains.mjs','compute-protocol.mjs','pkg/compute/tilo_browser_compute.js','pkg/compute/tilo_browser_compute_bg.wasm'])assets[name]=sha(readFileSync(join(output,name)));
  const receipt={assets,sourceCommit:execFileSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}).trim(),sourceDiffSha256:sha(execFileSync('git',['diff','HEAD'],{cwd:root})),sources,artifact:{sha256:sha(wasm),bytes:wasm.length,memory,imports:WebAssembly.Module.imports(new WebAssembly.Module(wasm))},result};
  writeFileSync(join(output,'qualification.json'),JSON.stringify(receipt,null,2));console.log(JSON.stringify(receipt,null,2));
}finally{if(browser)await browser.close();await server.close();}
