import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

// Exercise the mount's real event bindings; only WASM/GPU/storage are replaced.
class DomEvent {
 constructor(type,init={}){Object.assign(this,{type,defaultPrevented:false},init);}
 preventDefault(){this.defaultPrevented=true;}
 stopImmediatePropagation(){this.stopped=true;}
 stopPropagation(){this.stopped=true;}
}
class Node {
 constructor(tag){this.tagName=tag;this.style={};this.dataset={};this.children=[];this.listeners=new Map();this.value='';}
 append(...nodes){for(const node of nodes){node.parentNode=this;this.children.push(node);}}
 setAttribute(){} removeAttribute(){} getAttribute(){return null;}
 contains(target){return target===this||this.children.some(child=>child.contains(target));}
 closest(){return null;}
 addEventListener(type,listener,options={}){const rows=this.listeners.get(type)??[];rows.push({listener,options});this.listeners.set(type,rows);}
 dispatchEvent(event){
  event.target??=this;const path=[];for(let node=this;node;node=node.parentNode)path.push(node);
  for(const node of [...path].reverse())for(const row of node.listeners.get(event.type)??[])if(row.options.capture&&!row.options.signal?.aborted&&!event.stopped){event.currentTarget=node;row.listener(event);}
  for(const node of path)for(const row of node.listeners.get(event.type)??[])if(!row.options.capture&&!row.options.signal?.aborted&&!event.stopped){event.currentTarget=node;row.listener(event);}
  return !event.defaultPrevented;
 }
 focus(options){this.focusOptions=options;if(document.activeElement===this)return;document.activeElement?.dispatchEvent(new DomEvent('blur'));document.activeElement=this;this.dispatchEvent(new DomEvent('focus'));this.dispatchEvent(new DomEvent('focusin'));}
 cloneNode(){const next=new Node(this.tagName);Object.assign(next,{style:{...this.style},dataset:{...this.dataset},tabIndex:this.tabIndex,disabled:this.disabled});return next;}
 replaceWith(next){const parent=this.parentNode;parent.children[parent.children.indexOf(this)]=next;next.parentNode=parent;this.parentNode=null;if(document.activeElement===this)document.activeElement=document.body;}
 remove(){this.parentNode.children=this.parentNode.children.filter(child=>child!==this);}
 replaceChildren(){this.children=[];}
}
const tick=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};
async function fixture({standalone=false,legacy=false}={}){
 const document=new Node('document');document.body=new Node('body');document.append(document.body);document.activeElement=document.body;document.createElement=tag=>new Node(tag);globalThis.document=document;
 Object.defineProperty(globalThis,'navigator',{configurable:true,value:{gpu:{},platform:'Mac'}});
 globalThis.KeyboardEvent=DomEvent;globalThis.FocusEvent=DomEvent;globalThis.fetch=async url=>{
  if(String(url).endsWith('fonts.json'))return {ok:true,json:async()=>({schema:1,catalog:'host-input-fixture',assets:[{name:'NotoSans[wght].ttf',bytes:1,sha256:'4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a',startup:true}]})};
  return {ok:true,arrayBuffer:async()=>new Uint8Array([1]).buffer};
 };
 let allocated=false,ownershipReads=0;let state={sequence:'1',activation:'1',owner:'writer',ime:{x:0,y:0,height:20}};const delivered=[],deliveredOwners=[];let update;
 const runtime={mount(_canvas,_config,_assets,_entries,callback){update=callback;setTimeout(()=>{allocated=true;callback(JSON.stringify({phase:'gpu-pending'}));queueMicrotask(()=>callback(JSON.stringify({phase:'presented',frames:1})));},0);return 1;},refresh_storage_lease(){},pending_writes:()=> '[]',asset_requests:()=> '[]',modifiers(){},ime:(...args)=>delivered.push(args.slice(1)),clipboard_event:(_id,kind)=>{delivered.push([kind]);return true;},
  desktop_input_ownership:()=>{ownershipReads++;if(!allocated)throw Error('Desktop disposed');if(standalone)throw Error('Desktop navigation unavailable');return JSON.stringify(state);},desktop_navigation_snapshot:()=>{if(!allocated)throw Error('Desktop disposed');return standalone?undefined:JSON.stringify({generation:'g'});},desktop_navigation_revision:()=>1,
  desktop_navigate:(_id,raw)=>{const command=JSON.parse(raw);if(command.route==='bad')throw Error('rejected');state={sequence:String(Number(state.sequence)+1),activation:String(Number(state.activation)+1),owner:command.route,ime:command.route==='calculator'?null:{x:0,y:0,height:20}};},
 };
 if(legacy)delete runtime.desktop_input_ownership;
 const originalIme=runtime.ime;runtime.ime=(...args)=>{if(args[1]==='commit')deliveredOwners.push([state.owner,args[2]]);originalIme(...args);};
 globalThis.__hostFixture={runtime,storage:{fresh:false,hydrate:async()=>[],release:async()=>{}}};
 let source=(await readFile(new URL('./host.mjs',import.meta.url),'utf8')).replaceAll('import.meta.url',JSON.stringify(new URL('./host.mjs',import.meta.url).href));
 source=source.replace("import init,* as runtime from './pkg/qos_browser_runtime.js';","const init=async()=>{};const runtime=globalThis.__hostFixture.runtime;")
  .replace("import {acquireStorage} from './storage.mjs';","const acquireStorage=async()=>globalThis.__hostFixture.storage;")
  .replace(/from '(\.\/[^']+)'/g,(_all,path)=>`from '${new URL(path,import.meta.url).href}'`);
 const {mountDesktop}=await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}#${Math.random()}`);
 const target=new Node('div');document.body.append(target);const host=await mountDesktop(target,{app:standalone?'writer':'desktop'});await tick();
 return {host,document,delivered,deliveredOwners,get ownershipReads(){return ownershipReads;},disposeRuntime(){allocated=false;},get state(){return state;},set state(value){state=value;},update:state=>update(JSON.stringify(state))};
}
test('outside pointer press dismisses only the current system search and preserves the external click',async()=>{
 const f=await fixture();
 const keys=[];f.host.canvas.addEventListener('keydown',event=>keys.push(event.key));f.host.canvas.addEventListener('keyup',event=>keys.push(event.key));
 const outside=new Node('button');f.document.body.append(outside);
 f.state={...f.state,sequence:'2',owner:'system-search'};
 f.host.canvas.dispatchEvent(new DomEvent('pointerdown'));assert.deepEqual(keys,[]);
 const event=new DomEvent('pointerdown');outside.dispatchEvent(event);
 assert.deepEqual(keys,['Escape','Escape']);assert.equal(event.defaultPrevented,false);assert.equal(event.stopped,undefined);
 keys.length=0;f.state={...f.state,sequence:'3',owner:'writer'};
 outside.dispatchEvent(new DomEvent('pointerdown'));assert.deepEqual(keys,[]);
});
test('mounted activation focuses latest destination and preserves a later external gesture',async()=>{
 const f=await fixture();
 await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});assert.equal(f.document.activeElement,f.host.editor);assert.deepEqual(f.host.editor.focusOptions,{preventScroll:true});
 const first=f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});const second=f.host.navigation.dispatch({kind:'select',generation:'g',route:'calculator'});await Promise.all([first,second]);assert.equal(f.document.activeElement,f.host.canvas);
 const pending=f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});const external=new Node('textarea');f.document.body.append(external);external.focus();await pending;assert.equal(f.document.activeElement,external);
 await assert.rejects(f.host.navigation.dispatch({kind:'select',generation:'g',route:'bad'}),/rejected/);assert.equal(f.document.activeElement,external);
 f.update({phase:'presented',frames:2,ime:{x:0,y:0,height:20}});await tick();assert.equal(f.document.activeElement,external);
});
test('mounted canceled IME tail cannot commit through end, beforeinput or native input',async()=>{
 const f=await fixture();await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});f.delivered.length=0;
 let target=f.host.editor;const send=(type,init={})=>target.dispatchEvent(new DomEvent(type,init));
 send('compositionstart');send('compositionupdate',{data:'old'});await f.host.navigation.dispatch({kind:'select',generation:'g',route:'chat'});
 send('compositionend',{data:'old'});send('beforeinput',{inputType:'insertText',data:'old'});send('input',{data:'old'});
 assert.equal(f.delivered.some(([kind])=>kind==='commit'),false);
 target=f.host.editor;send('compositionstart');send('compositionupdate',{data:'new'});send('compositionend',{data:'new'});assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[['commit','new']]);
});
test('mounted dead key composes one caret and retains the following numeric input',async()=>{
 const f=await fixture();f.host.editor.focus();f.delivered.length=0;const old=f.host.editor,canvasKeys=[];
 f.host.canvas.addEventListener('keydown',event=>canvasKeys.push(event.key));
 const dead=new DomEvent('keydown',{key:'Dead',code:'Digit6'});old.dispatchEvent(dead);
 assert.equal(dead.defaultPrevented,false,'dead key remains native so Safari can start composition');
 old.dispatchEvent(new DomEvent('compositionstart'));old.dispatchEvent(new DomEvent('compositionupdate',{data:'^'}));old.dispatchEvent(new DomEvent('compositionend',{data:'^'}));
 assert.notEqual(f.host.editor,old,'the committed composition retires its editor');
 const staleTail=new DomEvent('beforeinput',{inputType:'insertCompositionText',data:'^'});old.dispatchEvent(staleTail);
 assert.equal(staleTail.defaultPrevented,true,'the retired editor rejects Safari composition tails');
 f.host.editor.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'2'}));
 assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[['commit','^'],['commit','2']]);
 assert.deepEqual(canvasKeys,[],'the dead key is not forwarded as a Winit command');
 assert.equal(f.host.editor.value,'','the hidden editor retains no committed value');
});
test('Rust-internal Search selection hands off only for a local surface gesture',async()=>{
 const f=await fixture();f.host.canvas.focus();
 f.host.canvas.addEventListener('keydown',()=>{f.state={sequence:'2',activation:'2',owner:'chat',ime:{x:0,y:0,height:20}};});
 f.host.canvas.dispatchEvent(new DomEvent('keydown',{key:'Enter'}));await tick();assert.equal(f.document.activeElement,f.host.editor);
 const external=new Node('textarea');f.document.body.append(external);external.focus();
 f.state={sequence:'3',activation:'3',owner:'writer',ime:{x:0,y:0,height:20}};f.update({phase:'presented',frames:2});await tick();assert.equal(f.document.activeElement,external);
});
test('complete pointer gesture retains its original owner until Notes then Chat receive beforeinput',async()=>{
 const f=await fixture();
 await f.host.navigation.dispatch({kind:'select',generation:'g',route:'chat'});
 const activation=f.state.activation,surface=f.host.canvas.parentNode;
 const clickCanvas=()=>{for(const type of ['pointerdown','pointerup','click'])f.host.canvas.dispatchEvent(new DomEvent(type));};
 let destination={sequence:'3',activation,owner:'notes',ime:{x:0,y:0,height:20}},frames=2;
 f.host.canvas.addEventListener('pointerdown',()=>{f.state=destination;f.update({phase:'presented',frames});});
 const chatControl=new Node('textarea');surface.append(chatControl);chatControl.focus();
 let leakedToChat=0;chatControl.addEventListener('beforeinput',()=>leakedToChat++);
 clickCanvas();await tick();
 assert.equal(f.state.activation,activation,'a direct control click does not select a desktop route');
 assert.equal(f.document.activeElement,f.host.editor,'the new Notes IME owner receives native DOM focus');
 f.document.activeElement.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'notes sentinel'}));
 assert.equal(leakedToChat,0);assert.deepEqual(f.deliveredOwners.at(-1),['notes','notes sentinel']);

 const notesControl=new Node('textarea');surface.append(notesControl);notesControl.focus();
 let leakedToNotes=0;notesControl.addEventListener('beforeinput',()=>leakedToNotes++);
 destination={sequence:'4',activation,owner:'chat',ime:{x:0,y:0,height:20}};frames=3;
 clickCanvas();await tick();
 assert.equal(f.document.activeElement,f.host.editor,'the returning Chat IME owner receives native DOM focus');
 f.document.activeElement.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'chat sentinel'}));
 assert.equal(leakedToNotes,0);assert.deepEqual(f.deliveredOwners.at(-1),['chat','chat sentinel']);
});
test('queued stale presentation IME cannot override fresh activation ownership',async()=>{
 const f=await fixture();await f.host.navigation.dispatch({kind:'select',generation:'g',route:'calculator'});
 f.update({phase:'presented',frames:2,ime:{x:0,y:0,height:20}});await tick();assert.equal(f.document.activeElement,f.host.canvas);
});
test('active native IME beforeinput remains native while canceled tails are consumed',async()=>{
 const f=await fixture();await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});
 f.host.editor.dispatchEvent(new DomEvent('compositionstart'));
 const active=new DomEvent('beforeinput',{inputType:'insertCompositionText',data:'preedit',isComposing:true});f.host.editor.dispatchEvent(active);assert.equal(active.defaultPrevented,false);
 f.host.editor.value='preedit';f.host.editor.dispatchEvent(new DomEvent('input',{isComposing:true}));assert.equal(f.host.editor.value,'preedit');
 const old=f.host.editor;await f.host.navigation.dispatch({kind:'select',generation:'g',route:'chat'});
 const canceled=new DomEvent('beforeinput',{inputType:'insertCompositionText',data:'preedit',isComposing:true});old.dispatchEvent(canceled);assert.equal(canceled.defaultPrevented,true);
});

test('ownership waits for asynchronous allocation and does not mask later disposal',async()=>{
 const f=await fixture();assert.ok(f.ownershipReads>0);f.disposeRuntime();
 await assert.rejects(f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'}),/Desktop disposed/);
});
test('current standalone and legacy runtimes retain local text input without desktop ownership',async()=>{
 for(const config of [{standalone:true},{legacy:true}]){
  const f=await fixture(config);f.host.editor.focus();f.host.editor.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'local'}));
  assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[['commit','local']]);assert.equal(f.ownershipReads,0);
 }
});
test('revoked hidden editor rejects delayed identical tails while fresh soft input works immediately',async()=>{
 for(const route of ['writer','chat']){
  const f=await fixture();await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});const old=f.host.editor;
  old.dispatchEvent(new DomEvent('compositionstart'));await f.host.navigation.dispatch({kind:'select',generation:'g',route});
  if(route==='chat')await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});
  old.dispatchEvent(new DomEvent('compositionend',{data:'same'}));await new Promise(resolve=>setTimeout(resolve,5));
  old.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'same'}));old.dispatchEvent(new DomEvent('input',{data:'same'}));
  assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[]);
  assert.notEqual(f.host.editor,old);f.host.editor.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'same'}));
  assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[['commit','same']]);
 }
});
test('asynchronous Rust Search activation retains local gesture authority but respects later outside focus',async()=>{
 for(const outside of [false,true]){
  const f=await fixture();const search=new Node('textarea');f.host.canvas.parentNode.append(search);search.focus();let complete;
  search.addEventListener('keydown',()=>{complete=()=>{f.state={sequence:'2',activation:'2',owner:'chat',ime:{x:0,y:0,height:20}};f.update({phase:'presented',frames:2});};});
  search.dispatchEvent(new DomEvent('keydown',{key:'Enter'}));await tick();
  const external=new Node('textarea');f.document.body.append(external);if(outside)external.focus();
  // Runtime allocation/event queues may finish after the initiating DOM turn.
  complete();await tick();assert.equal(f.document.activeElement,outside?external:f.host.editor);
 }
});
test('every retired hidden editor listener rejects old input and focus callbacks',async()=>{
 const f=await fixture();await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});const old=f.host.editor;
 old.dispatchEvent(new DomEvent('compositionstart'));await f.host.navigation.dispatch({kind:'select',generation:'g',route:'chat'});f.delivered.length=0;
 const keys=[];for(const type of ['keydown','keyup'])f.host.canvas.addEventListener(type,event=>keys.push(event.key));
 for(const type of ['paste','copy','cut','keydown','keyup','focus','blur','compositionstart','compositionupdate','compositionend','beforeinput','input'])old.dispatchEvent(new DomEvent(type,{key:'Enter',data:'old',inputType:'insertText',clipboardData:{getData:()=> 'old'}}));
 assert.deepEqual(f.delivered,[]);assert.deepEqual(keys,[]);
 f.host.editor.dispatchEvent(new DomEvent('paste',{clipboardData:{getData:()=> 'new'}}));assert.deepEqual(f.delivered,[['paste']]);
});
test('plain hidden editor lifetime is revoked by each exact counter and away/back',async()=>{
 for(const mode of ['same-owner','sequence','activation','away-back']){
  const f=await fixture();await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});const old=f.host.editor;f.delivered.length=0;
  if(mode==='away-back'){await f.host.navigation.dispatch({kind:'select',generation:'g',route:'chat'});await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});}
  else if(mode==='same-owner')await f.host.navigation.dispatch({kind:'select',generation:'g',route:'writer'});
  else{f.state={...f.state,[mode]:String(Number(f.state[mode])+1)};f.update({phase:'presented',frames:2});await tick();}
  old.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'stale'}));old.dispatchEvent(new DomEvent('input',{data:'stale'}));
  assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[],mode);
  assert.notEqual(f.host.editor,old);f.host.editor.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'fresh'}));
  assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[['commit','fresh']]);
 }
});
