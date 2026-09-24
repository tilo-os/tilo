import test from 'node:test';
import assert from 'node:assert/strict';
import {createInvitedIdentityCustodian} from './invited-identity-custody.mjs';
function fixture(){
 const held=new Set(),rows=new Map(),sessionRows=new Map();let creates=0,saves=0;
 const locks={async request(name,options,operation){if(held.has(name))return operation(null);held.add(name);try{return await operation({});}finally{held.delete(name);}}};
 const store={load:async key=>rows.get(key),save:async(key,value)=>{saves++;rows.set(key,value);}};
 const session={getItem:key=>sessionRows.get(key)??null,setItem:(key,value)=>sessionRows.set(key,value)};
 const runtime={invited_identity_scope:()=> 'scope',invited_identity_create:()=>JSON.stringify({device:`device-${++creates}`,scope:'scope',binding:'a'.repeat(64)}),invited_identity_public:bytes=>JSON.stringify({device_public_key:JSON.parse(bytes).device,scope:JSON.parse(bytes).scope,binding:JSON.parse(bytes).binding})};
 const open=createInvitedIdentityCustodian({runtime,locks,store});return{runtime,store,session,held,rows,open,counts:()=>({creates,saves})};
}
test('invited identity persists before use, retains exact owner on reload and never forks around an occupied lease',async()=>{
 const f=fixture();const lease=await f.open({text:'invite',identity:'new-person',label:'guest'},f.session);
 assert.equal(f.counts().saves,1);await assert.rejects(f.open({text:'invite',identity:'new-person',label:'guest'},f.session),/occupied/);assert.equal(f.counts().creates,1);
 await lease.save(JSON.stringify({device:'device-1',scope:'scope',binding:'a'.repeat(64),completion:'signed-exact'}));await lease.release();
 const restored=await f.open({text:'invite',identity:'new-person',label:'guest'},f.session);assert.equal(JSON.parse(restored.material).completion,'signed-exact');assert.equal(f.counts().creates,1);await restored.release();assert.equal(f.held.size,0);
});
test('same device cannot replace retained person/certificate/request binding or omit the binding',async()=>{
 const f=fixture(),lease=await f.open({text:'invite',identity:'new-person',label:'guest'},f.session);
 for(const binding of [undefined,'b'.repeat(64)])await assert.rejects(lease.save(JSON.stringify({device:'device-1',scope:'scope',binding})),/custody/);
 await lease.release();
 const missing=fixture();missing.runtime.invited_identity_public=()=>JSON.stringify({device_public_key:'device-1',scope:'scope'});
 await assert.rejects(missing.open({text:'invite',identity:'new-person',label:'guest'},missing.session),/custody/);assert.equal(missing.held.size,0);
});
test('failed initial ACK, changed device and changed invitation scope retain no usable lease',async()=>{
 const f=fixture();f.store.save=async()=>{throw Error('ACK failed');};await assert.rejects(f.open({text:'invite',identity:'new-person',label:'guest'},f.session),/ACK failed/);assert.equal(f.held.size,0);
 const good=fixture();const lease=await good.open({text:'invite',identity:'new-person',label:'guest'},good.session);
 await assert.rejects(lease.save(JSON.stringify({device:'other',scope:'scope'})),/custody/);
 await assert.rejects(lease.save(JSON.stringify({device:'device-1',scope:'changed'})),/custody/);await lease.release();
});
test('restarted browser adopts the stored invited identity for the same scope instead of redeeming again',async()=>{
 const f=fixture(),index=new Map();f.store.candidates=async locator=>[...(index.get(locator)??[])];f.store.remember=async(locator,key)=>{index.set(locator,[key]);};
 const invite={text:'invite',identity:'new-person',label:'guest'};
 const lease=await f.open(invite,f.session);const key=lease.publicKey;await lease.release();
 const fresh={getItem:()=>null,setItem:()=>{}};
 const restarted=await f.open(invite,fresh);assert.equal(restarted.publicKey,key);assert.equal(f.counts().creates,1);await restarted.release();assert.equal(f.held.size,0);
});
