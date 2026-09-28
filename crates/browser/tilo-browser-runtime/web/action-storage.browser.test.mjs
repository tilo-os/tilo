// Isolated real-browser transaction regression; no peer/native readiness claim.
import {createRequire} from 'node:module';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
const require=createRequire(import.meta.url),{chromium}=require('/Users/xaver/.npm/_npx/705bc6b22212b352/node_modules/playwright');
const root=new URL('./',import.meta.url);
const server=createServer(async(req,res)=>{
 try{
  if(req.url==='/'){res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>Action storage regression</title>');return;}
  res.setHeader('Content-Type','text/javascript');
  if(req.url==='/loss-worker.mjs'){res.end(`import {servePersistence} from './persistence-worker.mjs';
   const commits=new Set();const port={postMessage(message,transfer){if(commits.has(message.id)&&message.ok){self.postMessage({committedBeforeLoss:true});setTimeout(()=>{throw Error('test worker dies after actual strict commit');},0);return;}self.postMessage(message,transfer);}};
   servePersistence(port,{locks:navigator.locks,indexedDB,crypto});self.onmessage=e=>{if(e.data.type==='commit')commits.add(e.data.id);port.onmessage(e);};`);return;}
  if(!/^\/[a-z0-9-]+\.mjs$/.test(req.url))throw Error('not found');
  res.end(await readFile(new URL(req.url.slice(1),root)));
 }catch{res.statusCode=404;res.end();}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try{
 browser=await chromium.launch({executablePath:'/Users/xaver/Library/Caches/ms-playwright/chromium-1243/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',headless:true});
 const page=await browser.newPage();await page.goto(`http://127.0.0.1:${server.address().port}/`);
 const result=await page.evaluate(async()=>{
  const {createStorageAccess}=await import('/storage.mjs'),{createActionStorage}=await import('/action-storage.mjs'),{createActionClient}=await import('/action-client.mjs');
  const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',id='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',second='cccccccc-cccc-4ccc-8ccc-cccccccccccc';
  const binding={room:'room',sender:'device',senderRoot:'person',target:'governor',targetRoot:'workspace',certificate:'exact'};
  const route={app_peer:'7',app_id:'writer',record_id:'r',resource:'document',action:'insert',revision:'current'};
  const catalogue={version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'r',actions:[{resource:'document',action:'insert',revision:'current',contract:{confirmation:{mode:'none'}}}]}]};
  let committedMarker=false;const access=createStorageAccess({createWorker:()=>{const worker=new Worker('/loss-worker.mjs',{type:'module'});worker.addEventListener('message',e=>{if(e.data.committedBeforeLoss)committedMarker=true;});return worker;},onFallback:()=>{}});
  const lease=await access.acquireStorage('isolated-regression',crypto.randomUUID()),epoch=lease.epoch,key=JSON.stringify({type:'privateActions',workspace:'room'});
  const persistence=createActionStorage({storage:lease,key});const calls=[];
  const client=await createActionClient({...persistence,context:()=>binding,request:async m=>{calls.push(structuredClone(m));return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}});
  let error;try{await client.invoke({...route,invocation:id});}catch(e){error=e.name;}
  const retainedBefore=(await lease.hydrate()).find(r=>r.key===key);const original=JSON.parse(new TextDecoder().decode(retainedBefore.bytes)).entries[0].intent;
  await client.driveRetained(id);
  lease.abortNext=true;let abortName,abortMessage;try{await client.invoke({...route,invocation:second});}catch(e){abortName=e.name;abortMessage=e.message;}
  const afterAbort=JSON.parse(new TextDecoder().decode((await lease.hydrate()).find(r=>r.key===key).bytes));
  await client.invoke({...route,invocation:second});
  const stored=(await lease.hydrate()).find(r=>r.key===key),entries=JSON.parse(new TextDecoder().decode(stored.bytes)).entries;
  await client.close();await lease.release();
  return {committedMarker,error,abortName,abortMessage,abortPreserved:afterAbort.entries.length===1&&afterAbort.entries[0].invocation===id,epochPreserved:stored.epoch===epoch,ids:entries.map(e=>e.invocation),original,resent:calls.filter(m=>m.type==='invoke_bound'&&m.invocation===id),mode:access.mode(),browser:navigator.userAgent};
 });
 assert.equal(result.committedMarker,true);assert.equal(result.error,'StorageWorkerError');assert.equal(result.epochPreserved,true);assert.equal(result.mode,'inline');assert.notEqual(result.abortName,'StorageWorkerError');assert.match(result.abortMessage,/aborted/i);assert.equal(result.abortPreserved,true);assert.equal(result.ids.length,2);assert.deepEqual(result.resent,[result.original]);
 const tickets=await page.evaluate(async()=>{
  const {createStorageAccess}=await import('/storage.mjs'),{createActionStorage}=await import('/action-storage.mjs'),{createActionClient}=await import('/action-client.mjs');
  const profile='ticket-regression',device=crypto.randomUUID(),key=JSON.stringify({type:'privateActions',workspace:'ticket-room'}),binding={room:'ticket-room',sender:'device',senderRoot:'person',target:'governor',targetRoot:'workspace',certificate:'exact'},host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
  const route={app_peer:'7',app_id:'writer',record_id:'r',resource:'document',action:'insert',revision:'current',argument:{text:'original retained bytes'}};
  const catalogue={version:1,surfaces:[{peer_id:'7',cluster_id:'ticket-room',app_id:'writer',record_id:'r',actions:[{resource:'document',action:'insert',revision:'current',contract:{confirmation:{mode:'none'}}}]}]};
  const seed=createStorageAccess({createWorker:()=>null}),seedLease=await seed.acquireStorage(profile,device),seedPersistence=createActionStorage({storage:seedLease,key});
  const seeded=await createActionClient({version:2,...seedPersistence,context:()=>binding,request:async()=>{throw Error('promotion must not send');}});await seeded.close();await seedLease.release();
  let committedMarker=false;const access=createStorageAccess({createWorker:()=>{const worker=new Worker('/loss-worker.mjs',{type:'module'});worker.addEventListener('message',e=>{if(e.data.committedBeforeLoss)committedMarker=true;});return worker;},onFallback:()=>{}});
  const lease=await access.acquireStorage(profile,device),epoch=lease.epoch,persistence=createActionStorage({storage:lease,key}),calls=[];
  const client=await createActionClient({version:2,...persistence,context:()=>binding,request:async m=>{calls.push(m);return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}});
  let error;try{await client.actionsV2.beginNew(route,{consumer:{kind:'interactive',parent:crypto.randomUUID(),slot:'idb-result'}});}catch(e){error=e.name;}
  const attempted=(await lease.hydrate()).find(row=>row.key===key),originalText=new TextDecoder().decode(attempted.bytes),original=JSON.parse(originalText).entries[0];
  await client.actionsV2.catalogue();
  const reconciled=(await lease.hydrate()).find(row=>row.key===key),ticket=client.actionsV2.getSnapshot().entries[0].ticket;
  const exactBytes=originalText===new TextDecoder().decode(reconciled.bytes),sameEpoch=epoch===reconciled.epoch,noFirstSend=calls.every(m=>m.type==='catalogue_refresh');
  await client.close();await lease.release();
  localStorage.setItem('ticket-regression',JSON.stringify({profile,device,key,binding,host,route,catalogue,ticket,original}));
  return {committedMarker,error,exactBytes,sameEpoch,noFirstSend,ticket,original};
 });
 assert.equal(tickets.committedMarker,true);assert.equal(tickets.error,'StorageWorkerError');assert.equal(tickets.exactBytes,true);assert.equal(tickets.sameEpoch,true);assert.equal(tickets.noFirstSend,true);
 await page.reload();
 const reopened=await page.evaluate(async()=>{
  const {createStorageAccess}=await import('/storage.mjs'),{createActionStorage}=await import('/action-storage.mjs'),{createActionClient}=await import('/action-client.mjs');
  const saved=JSON.parse(localStorage.getItem('ticket-regression')),access=createStorageAccess({createWorker:()=>null}),lease=await access.acquireStorage(saved.profile,saved.device),persistence=createActionStorage({storage:lease,key:saved.key});
  const calls=[];let invoked=false,peakRows=0,peakBytes=0,ackBeforeRemoval=false;
  const request=async m=>{calls.push(structuredClone(m));if(m.type==='catalogue_refresh')return{type:'broker_catalogue',host:saved.host,catalogue:saved.catalogue};if(m.type==='invoke_bound')invoked=true;return{type:'observation',host:saved.host,invocation:m.invocation,update:invoked?{state:'succeeded',invocation:m.invocation,output:{resources:[],value:{persisted:true}}}:null,uncertain:false,cancellation_requested:false};};
  const client=await createActionClient({version:2,...persistence,context:()=>saved.binding,request}),a=client.actionsV2;
  const reloadOriginal=JSON.stringify(a.getSnapshot().entries[0].intent)===JSON.stringify(saved.original.intent),reloadTicket=JSON.stringify(a.getSnapshot().entries[0].ticket)===JSON.stringify(saved.ticket);
  const row=await a.drive(saved.ticket);
  await a.acknowledgeTerminal(saved.ticket,{consumer:saved.ticket.consumer,update:row.update,continuation:{kind:'idb-ui-result-presented'}});
  const before=JSON.parse(new TextDecoder().decode((await lease.hydrate()).find(row=>row.key===saved.key).bytes));ackBeforeRemoval=!!before.entries[0].consumerAcknowledgement;
  await a.consumeTerminal(saved.ticket);
  const noRpcStart=calls.length;
  for(const ticket of [saved.ticket,{...saved.ticket,namespace:crypto.randomUUID()},{...saved.ticket,operation:crypto.randomUUID()}])for(const method of ['drive','observe','requestCancellation','consumeTerminal']){try{await a[method](ticket);throw Error('missing ticket accepted');}catch(error){if(error.message==='missing ticket accepted')throw error;}}
  const missingNoRpc=calls.length===noRpcStart;
  for(let i=0;i<270;i++){
    const ticket=await a.beginNew(saved.route,{consumer:{kind:'interactive',parent:crypto.randomUUID(),slot:'sequential-ui'}}),result=await a.observe(ticket);
    await a.acknowledgeTerminal(ticket,{consumer:ticket.consumer,update:result.update,continuation:{kind:'idb-ui-result-presented'}});
    const bytes=(await lease.hydrate()).find(row=>row.key===saved.key).bytes;peakBytes=Math.max(peakBytes,bytes.byteLength);peakRows=Math.max(peakRows,JSON.parse(new TextDecoder().decode(bytes)).entries.length);
    await a.consumeTerminal(ticket);
  }
  const finalRows=a.getSnapshot().entries.length;await client.close();await lease.release();localStorage.removeItem('ticket-regression');
  return{reloadOriginal,reloadTicket,ackBeforeRemoval,missingNoRpc,peakRows,peakBytes,finalRows,sequential:270,resent:calls.filter(m=>m.type==='invoke_bound')};
 });
 assert.equal(reopened.reloadOriginal,true);assert.equal(reopened.reloadTicket,true);assert.equal(reopened.ackBeforeRemoval,true);assert.equal(reopened.missingNoRpc,true);assert.equal(reopened.peakRows,1);assert.ok(reopened.peakBytes<4*1024*1024);assert.equal(reopened.finalRows,0);assert.deepEqual(reopened.resent,[tickets.original.intent]);
 const removal=await page.evaluate(async()=>{
  const {createStorageAccess}=await import('/storage.mjs'),{createActionStorage}=await import('/action-storage.mjs'),{createActionClient}=await import('/action-client.mjs');
  const profile='ticket-removal',device=crypto.randomUUID(),key=JSON.stringify({type:'privateActions',workspace:'removal'}),host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',binding={room:'removal',sender:'device',senderRoot:'person',target:'governor',targetRoot:'workspace',certificate:'exact'};
  const route={app_peer:'7',app_id:'writer',record_id:'r',resource:'document',action:'insert',revision:'current'},catalogue={version:1,surfaces:[{peer_id:'7',cluster_id:'removal',app_id:'writer',record_id:'r',actions:[{resource:'document',action:'insert',revision:'current',contract:{confirmation:{mode:'none'}}}]}]};
  const initial=createStorageAccess({createWorker:()=>null}),seedLease=await initial.acquireStorage(profile,device);
  const seed=await createActionClient({version:2,...createActionStorage({storage:seedLease,key}),context:()=>binding,request:async m=>m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation:m.invocation,update:{state:'succeeded',invocation:m.invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false}});
  const ticket=await seed.actionsV2.beginNew(route,{consumer:{kind:'interactive',parent:crypto.randomUUID(),slot:'removal'}}),row=await seed.actionsV2.observe(ticket);
  await seed.actionsV2.acknowledgeTerminal(ticket,{consumer:ticket.consumer,update:row.update,continuation:{kind:'result-presented'}});await seed.close();await seedLease.release();
  let committedMarker=false;const access=createStorageAccess({createWorker:()=>{const worker=new Worker('/loss-worker.mjs',{type:'module'});worker.addEventListener('message',e=>{if(e.data.committedBeforeLoss)committedMarker=true;});return worker;},onFallback:()=>{}}),lease=await access.acquireStorage(profile,device),epoch=lease.epoch;
  let rpc=0;const client=await createActionClient({version:2,...createActionStorage({storage:lease,key}),context:()=>binding,request:async()=>{rpc++;throw Error('missing ticket must not send');}});let error,rejected=0;
  try{await client.actionsV2.consumeTerminal(ticket);}catch(e){error=e.name;}
  const actual=(await lease.hydrate()).find(row=>row.key===key),bytes=new TextDecoder().decode(actual.bytes);
  for(const method of ['drive','observe','requestCancellation','consumeTerminal']){try{await client.actionsV2[method](ticket);}catch{rejected++;}}
  const reconciled=(await lease.hydrate()).find(row=>row.key===key),result={committedMarker,error,rpc,rejected,missingRows:JSON.parse(bytes).entries.length,sameEpoch:reconciled.epoch===epoch,exactBytes:bytes===new TextDecoder().decode(reconciled.bytes),liveRows:client.actionsV2.getSnapshot().entries.length};
  await client.close();await lease.release();return result;
 });
 assert.equal(removal.committedMarker,true);assert.equal(removal.error,'StorageWorkerError');assert.equal(removal.rpc,0);assert.equal(removal.rejected,4);assert.equal(removal.missingRows,0);assert.equal(removal.liveRows,0);assert.equal(removal.sameEpoch,true);assert.equal(removal.exactBytes,true);
 console.log(JSON.stringify({scope:'v2 lost strict removal acknowledgement reconciles exact empty row without RPC',...removal},null,2));
 console.log(JSON.stringify({scope:'v2 actual strict IndexedDB ticket admission ACK loss, same-lease reconciliation, page reload, explicit consumer acknowledgement and 270 sequential removals',tickets,reopened},null,2));
 console.log(JSON.stringify({scope:'actual strict IndexedDB worker commit then lost acknowledgement; same lease retained drive',...result},null,2));
}finally{await browser?.close();await new Promise(r=>server.close(r));}
