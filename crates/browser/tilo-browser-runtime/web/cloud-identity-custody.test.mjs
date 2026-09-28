import test from 'node:test';
import assert from 'node:assert/strict';
import {createCloudIdentityCustodian} from './cloud-identity-custody.mjs';
const SCOPE='a'.repeat(64),BINDING='b'.repeat(64);
const device=n=>n.toString(16).padStart(64,'0');
function fixture(){
 const held=new Set(),rows=new Map(),sessionRows=new Map();let creates=0,saves=0;const seen=[];
 const locks={async request(name,options,operation){if(held.has(name))return operation(null);held.add(name);try{return await operation({});}finally{held.delete(name);}}};
 const store={load:async key=>rows.get(key),save:async(key,value)=>{saves++;rows.set(key,value);}};
 const session={getItem:key=>sessionRows.get(key)??null,setItem:(key,value)=>sessionRows.set(key,value)};
 const runtime={
  cloud_identity_scope:(account,machine)=>{seen.push(['scope',account,machine]);return SCOPE;},
  cloud_identity_create:(account,machine,label)=>{seen.push(['create',account,machine,label]);return JSON.stringify({device:device(++creates),scope:SCOPE,binding:BINDING});},
  cloud_identity_public:(bytes,account,machine)=>{seen.push(['public',account,machine]);const v=JSON.parse(bytes);return JSON.stringify({device_public_key:v.device,scope:v.scope,binding:v.binding,step:v.step??'inspect'});},
 };
 const open=createCloudIdentityCustodian({runtime,locks,store});return{runtime,store,session,held,rows,open,seen,counts:()=>({creates,saves})};
}
const cloud={accountId:'acct',machineId:'m-1',label:'Owner browser',enrollment:{}};
test('owner identity persists before use, is exclusive, and restores the same custody on reload',async()=>{
 const f=fixture();const lease=await f.open(cloud,f.session);
 assert.equal(f.counts().saves,1);assert.deepEqual(f.seen.find(v=>v[0]==='create'),['create','acct','m-1','Owner browser']);
 await assert.rejects(f.open(cloud,f.session),error=>error.code==='open-in-other-tab');assert.equal(f.counts().creates,1);
 await lease.save(JSON.stringify({device:device(1),scope:SCOPE,binding:BINDING,step:'enrolled'}));await lease.release();
 const restored=await f.open(cloud,f.session);assert.equal(JSON.parse(restored.material).step,'enrolled');assert.equal(f.counts().creates,1);await restored.release();assert.equal(f.held.size,0);
});
test('same device cannot replace the retained owner binding or device',async()=>{
 const f=fixture(),lease=await f.open(cloud,f.session);
 await assert.rejects(lease.save(JSON.stringify({device:device(1),scope:SCOPE,binding:'c'.repeat(64)})),/custody/);
 await assert.rejects(lease.save(JSON.stringify({device:device(9),scope:SCOPE,binding:BINDING})),/custody/);
 await assert.rejects(lease.save(JSON.stringify({device:device(1),scope:'d'.repeat(64),binding:BINDING})),/custody/);
 await lease.release();assert.equal(f.held.size,0);
});
test('failed initial save and invalid selections retain no lease',async()=>{
 const f=fixture();f.store.save=async()=>{throw Error('ACK failed');};await assert.rejects(f.open(cloud,f.session),/ACK failed/);assert.equal(f.held.size,0);
 const g=fixture();
 for(const bad of [{...cloud,accountId:''},{...cloud,machineId:'x'.repeat(257)},{...cloud,label:' '},undefined])await assert.rejects(g.open(bad,g.session),TypeError);
 assert.equal(g.counts().creates,0);
});
test('restarted browser adopts the stored owner identity instead of creating a second first claim',async()=>{
 const f=fixture(),index=new Map();f.store.candidates=async locator=>[...(index.get(locator)??[])];f.store.remember=async(locator,key)=>{index.set(locator,[key]);};
 const lease=await f.open(cloud,f.session);const key=lease.publicKey;await lease.release();
 const restarted=await f.open(cloud,{getItem:()=>null,setItem:()=>{}});assert.equal(restarted.publicKey,key);assert.equal(f.counts().creates,1);await restarted.release();assert.equal(f.held.size,0);
});
