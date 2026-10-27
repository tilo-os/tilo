// Production account storage in one origin, two authenticated profile Dir scopes.
// Run: node crates/system/accounts/web/run.mjs [--no-build] [--no-sandbox]
import {execFileSync} from 'node:child_process';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
import {launch, serve} from '../../../../tools/web/chrome.mjs';

const repo=join(dirname(fileURLToPath(import.meta.url)),'../../../..');
const out=mkdtempSync(join(tmpdir(),'accounts-storage-'));
const cargo=(...args)=>execFileSync('cargo',args,{cwd:repo,stdio:['ignore','pipe','inherit'],maxBuffer:32*1024*1024}).toString();
let server,browser;
try{
if(!process.argv.includes('--no-build'))cargo('build','--locked','-p','accounts','--example','account_storage_probe','--target','wasm32-unknown-unknown');
const target=JSON.parse(cargo('metadata','--locked','--format-version','1','--no-deps','--offline')).target_directory;
execFileSync(process.env.WASM_BINDGEN??'wasm-bindgen',[
  join(target,'wasm32-unknown-unknown/debug/examples/account_storage_probe.wasm'),
  '--target','web','--out-dir',out,'--out-name','probe',
],{stdio:'inherit'});
writeFileSync(join(out,'index.html'),`<!doctype html><script type="module">
import init,* as probe from './probe.js'; await init(); globalThis.probe=probe; globalThis.probeReady=true;
</script>`);
server=await serve(out);
browser=await launch({args:process.argv.includes('--no-sandbox')?['--no-sandbox']:[]});
const tab=async()=>{const t=await browser.tab();await t.navigate(server.url+'/index.html');await t.waitFor('globalThis.probeReady === true');return t;};
const invoke=(t,name,...args)=>t.evaluate(`Promise.resolve(probe.${name}(...${JSON.stringify(args)})).then(value=>value??null).catch(error=>{throw new Error(String(error));})`);
const attempt=(t,handle)=>t.evaluate(`(()=>{try{probe.save(${JSON.stringify(handle)});return 'committed';}catch(e){return String(e);}})()`);
const commit=async(t,profile,handle)=>{
  assert.match(await attempt(t,handle),/save pending/);
  await invoke(t,'flush',profile);
  assert.equal(await attempt(t,handle),'committed');
};
const profileA='authenticated-public-key-a@host',profileB='authenticated-public-key-b@host';
  let a=await tab(),b=await tab();
  await invoke(a,'seed',profileA,'legacy-a');
  await invoke(a,'seed',profileB,'legacy-b');
  await invoke(a,'ambiguous_inventory');
  await invoke(a,'open',profileA,'a');
  await invoke(b,'open',profileB,'b');
  assert.equal(await invoke(a,'get','a','legacy'),'legacy-a');
  assert.equal(await invoke(b,'get','b','legacy'),'legacy-b');
  assert.equal(await invoke(a,'get','a','ambiguous'),null);
  assert.equal(await invoke(b,'get','b','ambiguous'),null);
  await invoke(a,'edit','a','only-a','secret-a');
  await commit(a,profileA,'a');
  // A's save must neither hide B's legacy file nor select A's inventory for B.
  await b.reload();await b.waitFor('globalThis.probeReady === true');
  await invoke(b,'open',profileB,'b');
  assert.equal(await invoke(b,'get','b','legacy'),'legacy-b');
  assert.equal(await invoke(b,'get','b','only-a'),null);
  await invoke(b,'edit','b','legacy','edited-b');
  await invoke(b,'edit','b','only-b','secret-b');
  await commit(b,profileB,'b');
  await a.reload();await a.waitFor('globalThis.probeReady === true');
  await invoke(a,'open',profileA,'a');
  assert.equal(await invoke(a,'get','a','legacy'),'legacy-a');
  assert.equal(await invoke(a,'get','a','only-a'),'secret-a');
  assert.equal(await invoke(a,'get','a','only-b'),null);
  await b.reload();await b.waitFor('globalThis.probeReady === true');
  await invoke(b,'open',profileB,'b');
  assert.equal(await invoke(b,'get','b','legacy'),'edited-b');
  assert.equal(await invoke(b,'get','b','only-b'),'secret-b');
  assert.equal(await invoke(b,'get','b','only-a'),null);
  assert.equal(await invoke(b,'ambiguous_retained'),true);
  assert.equal(await invoke(a,'encrypted',profileA),true);
  assert.equal(await invoke(b,'encrypted',profileB),true);
  console.log('ok profile-bound legacy read/edit/reload; unassigned inventory retained');

  // Two stale views of one profile merge disjoint edits, including across tabs.
  await invoke(b,'open',profileA,'peer');
  await invoke(a,'edit','a','concurrent-a','value-a');
  await invoke(b,'edit','peer','concurrent-b','value-b');
  assert.match(await attempt(a,'a'),/save pending/);
  assert.match(await attempt(b,'peer'),/save pending/);
  await Promise.all([invoke(a,'flush',profileA),invoke(b,'flush',profileA)]);
  assert.equal(await attempt(a,'a'),'committed');
  assert.equal(await attempt(b,'peer'),'committed');
  await a.reload();await a.waitFor('globalThis.probeReady === true');
  await invoke(a,'open',profileA,'a');
  assert.equal(await invoke(a,'get','a','concurrent-a'),'value-a');
  assert.equal(await invoke(a,'get','a','concurrent-b'),'value-b');
  console.log('ok same-profile concurrent merge');

  // A newer edit arriving before acknowledgement is rebased, not discarded.
  await invoke(a,'edit','a','midflight','first');
  assert.match(await attempt(a,'a'),/save pending/);
  await invoke(a,'edit','a','midflight','second');
  await invoke(a,'flush',profileA);
  assert.match(await attempt(a,'a'),/retry remaining edits/);
  await commit(a,profileA,'a');
  console.log('ok edits retained across asynchronous acknowledgement');

  // Fail the real OPFS write. A failed flush and consumed failure cannot claim success.
  await a.evaluate(`globalThis.originalCreateWritable=FileSystemFileHandle.prototype.createWritable;
    FileSystemFileHandle.prototype.createWritable=function(){return Promise.reject(new Error('injected credential write failure'));};`);
  await invoke(a,'edit','a','failed-write','first-refused');
  assert.match(await attempt(a,'a'),/save pending/);
  await invoke(a,'edit','a','failed-write','retained');
  await assert.rejects(invoke(a,'flush',profileA),/injected credential write failure/);
  assert.match(await attempt(a,'a'),/injected credential write failure/);
  assert.equal(await invoke(a,'get','a','failed-write'),'retained');
  await a.evaluate('FileSystemFileHandle.prototype.createWritable=globalThis.originalCreateWritable');
  assert.match(await attempt(a,'a'),/save pending/);
  await invoke(a,'flush',profileA);
  assert.match(await attempt(a,'a'),/retry remaining edits/);
  await commit(a,profileA,'a');
  await a.reload();await a.waitFor('globalThis.probeReady === true');
  await invoke(a,'open',profileA,'a');
  assert.equal(await invoke(a,'get','a','failed-write'),'retained');
  assert.equal(await invoke(a,'get','a','midflight'),'second');
  assert.equal(await invoke(a,'get','a','concurrent-b'),'value-b');
  console.log('ok failed writes preserve edits and existing encrypted inventory');

  // Same-entry conflicts do not overwrite the winner or discard the losing local edit.
  await invoke(b,'open',profileA,'stale');
  await invoke(a,'edit','a','legacy','winner');
  await invoke(b,'edit','stale','legacy','loser');
  await commit(a,profileA,'a');
  assert.match(await attempt(b,'stale'),/save pending/);
  await assert.rejects(invoke(b,'flush',profileA),/changed concurrently/);
  assert.match(await attempt(b,'stale'),/changed concurrently/);
  assert.equal(await invoke(b,'get','stale','legacy'),'loser');
  await a.reload();await a.waitFor('globalThis.probeReady === true');
  await invoke(a,'open',profileA,'a');
  assert.equal(await invoke(a,'get','a','legacy'),'winner');
  console.log('ok conflicting edits fail closed');

  const freshAttempt=()=>a.evaluate(`(()=>{try{probe.save_fresh(${JSON.stringify(profileA)},'fresh-facade','durable');return 'committed';}catch(e){return String(e);}})()`);
  assert.match(await freshAttempt(),/save pending/);
  await invoke(a,'flush',profileA);
  // The mirror now contains the new value, so this fresh vault has a different
  // baseline. It must still consume the original operation's acknowledgement.
  assert.equal(await freshAttempt(),'committed');
  assert.equal(await invoke(a,'pending',profileA),0);
  // A consumed operation is not a reusable success cache.
  assert.match(await freshAttempt(),/save pending/);
  await invoke(a,'flush',profileA);
  assert.equal(await freshAttempt(),'committed');
  assert.equal(await invoke(a,'pending',profileA),0);
  console.log('ok fresh synchronous facades consume acknowledgement exactly once');

  // A failed first write must remove its uncommitted empty OPFS handle so the
  // same local edits can retry, rather than treating it as a damaged vault.
  const newProfile='authenticated-new-user@host';
  await invoke(a,'open',newProfile,'new');
  await invoke(a,'edit','new','first','retry-new');
  await a.evaluate(`globalThis.originalCreateWritable=FileSystemFileHandle.prototype.createWritable;
    FileSystemFileHandle.prototype.createWritable=function(){return Promise.reject(new Error('first write refused'));};`);
  assert.match(await attempt(a,'new'),/save pending/);
  await assert.rejects(invoke(a,'flush',newProfile),/first write refused/);
  assert.match(await attempt(a,'new'),/first write refused/);
  await a.evaluate('FileSystemFileHandle.prototype.createWritable=globalThis.originalCreateWritable');
  await commit(a,newProfile,'new');
  await a.reload();await a.waitFor('globalThis.probeReady === true');
  await invoke(a,'open',newProfile,'new');
  assert.equal(await invoke(a,'get','new','first'),'retry-new');
  assert.equal(await invoke(a,'pending',newProfile),0);
  console.log('ok failed first writes preserve retryability');

  // A password saved while the browser still writes must stay saved, not be
  // undone because its first write is still in flight.
  const storeProfile='authenticated-passwords@host';
  await invoke(a,'store_open',storeProfile,'items');
  // Hold the browser's writes until the test has seen the save in flight:
  // otherwise the write can land before the store is asked how it ended.
  await a.evaluate(`globalThis.originalCreateWritable=FileSystemFileHandle.prototype.createWritable;
    globalThis.heldWrites=new Promise(release=>{globalThis.releaseWrites=release;});
    FileSystemFileHandle.prototype.createWritable=function(...args){
      return globalThis.heldWrites.then(()=>globalThis.originalCreateWritable.apply(this,args));
    };`);
  await invoke(a,'store_put','items','Bank');
  assert.deepEqual(await invoke(a,'store_titles','items'),['Bank']);
  assert.equal(await invoke(a,'store_flush','items'),'pending');
  await a.evaluate(`FileSystemFileHandle.prototype.createWritable=globalThis.originalCreateWritable;
    globalThis.releaseWrites();`);
  await invoke(a,'flush',storeProfile);
  assert.equal(await invoke(a,'store_flush','items'),'durable');
  await a.reload();await a.waitFor('globalThis.probeReady === true');
  await invoke(a,'store_open',storeProfile,'items');
  assert.deepEqual(await invoke(a,'store_titles','items'),['Bank']);
  console.log('ok saved items survive the browser write and a reload');
}finally{
  try{await browser?.close();}finally{
    await server?.close();rmSync(out,{recursive:true,force:true});
  }
}
