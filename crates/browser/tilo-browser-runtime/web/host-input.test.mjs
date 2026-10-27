import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createPanorama} from './panorama.mjs';
import {draggingFiles,droppedFiles,pastedPicture,MAX_DROPPED_FILE,MAX_DROPPED_FILES,MAX_PASTED_PICTURE} from './host-core.mjs';

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
 focus(options){this.focusOptions=options;if(document.activeElement===this)return;const previous=document.activeElement;previous?.dispatchEvent(new DomEvent('blur',{relatedTarget:this}));document.activeElement=this;this.dispatchEvent(new DomEvent('focus',{relatedTarget:previous}));this.dispatchEvent(new DomEvent('focusin',{relatedTarget:previous}));}
 cloneNode(){const next=new Node(this.tagName);Object.assign(next,{style:{...this.style},dataset:{...this.dataset},tabIndex:this.tabIndex,disabled:this.disabled});return next;}
 replaceWith(next){const parent=this.parentNode;parent.children[parent.children.indexOf(this)]=next;next.parentNode=parent;this.parentNode=null;if(document.activeElement===this)document.activeElement=document.body;}
 remove(){this.parentNode.children=this.parentNode.children.filter(child=>child!==this);}
 replaceChildren(){this.children=[];}
}
const tick=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};
async function fixture({standalone=false,legacy=false,models,providerManagement,app}={}){
 const document=new Node('document');document.body=new Node('body');document.append(document.body);document.activeElement=document.body;document.createElement=tag=>new Node(tag);globalThis.document=document;
 Object.defineProperty(globalThis,'navigator',{configurable:true,value:{gpu:{},platform:'Mac'}});
 globalThis.KeyboardEvent=DomEvent;globalThis.FocusEvent=DomEvent;globalThis.WheelEvent=class extends DomEvent{constructor(type,init){super(type,{deltaX:0,deltaY:0,deltaMode:0,...init});}};globalThis.fetch=async url=>{
  if(String(url).endsWith('fonts.json'))return {ok:true,json:async()=>({schema:1,catalog:'host-input-fixture',assets:[{name:'NotoSans[wght].ttf',bytes:1,sha256:'4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a',startup:true}]})};
  return {ok:true,arrayBuffer:async()=>new Uint8Array([1]).buffer};
 };
 let allocated=false,ownershipReads=0;let state={sequence:'1',activation:'1',owner:'writer',ime:{x:0,y:0,height:20}};const delivered=[],deliveredOwners=[],windowFocus=[];let update;
 // Winit's web backend reports the canvas's own focus and blur as the window's.
 const runtime={provide_startup_font(){},mount(canvas,_config,_entries,callback){this.config=JSON.parse(_config);update=callback;for(const type of ['focus','blur'])canvas.addEventListener(type,()=>windowFocus.push(type));setTimeout(()=>{allocated=true;callback(JSON.stringify({phase:'gpu-pending'}));queueMicrotask(()=>callback(JSON.stringify({phase:'presented',frames:1})));},0);return 1;},close:()=>[],refresh_storage_lease(){},load_storage(){},mirror_files:async()=>{},load_application:async()=>true,flush_files:async()=>{},storage_write_hook(){},provide_settings:()=>false,drain_storage:()=>[],retry_storage:()=>0,acknowledge_storage(){},pending_writes:()=>[],asset_requests:()=> '[]',modifiers(){},ime:(...args)=>delivered.push(args.slice(1)),clipboard_event:(_id,kind)=>{delivered.push([kind]);return true;},

  desktop_input_ownership:()=>{ownershipReads++;if(!allocated)throw Error('Desktop disposed');if(standalone)throw Error('Desktop navigation unavailable');return JSON.stringify(state);},desktop_navigation_snapshot:()=>{if(!allocated)throw Error('Desktop disposed');return standalone?undefined:JSON.stringify({generation:'g'});},desktop_navigation_revision:()=>1,searchToggles:0,desktop_search_toggle(){this.searchToggles++;},
  desktop_navigate:(_id,raw)=>{const command=JSON.parse(raw);if(command.route==='bad')throw Error('rejected');state={sequence:String(Number(state.sequence)+1),activation:String(Number(state.activation)+1),owner:command.route,ime:command.route==='calculator'?null:{x:0,y:0,height:20}};},
 };
 if(legacy)delete runtime.desktop_input_ownership;
 const originalIme=runtime.ime;runtime.ime=(...args)=>{if(args[1]==='commit')deliveredOwners.push([state.owner,args[2]]);originalIme(...args);};
 globalThis.__hostFixture={runtime,storage:{fresh:false,hydrate:async()=>[],commit:async()=>{},release:async()=>{}}};
 let source=(await readFile(new URL('./host.mjs',import.meta.url),'utf8')).replaceAll('import.meta.url',JSON.stringify(new URL('./host.mjs',import.meta.url).href));
 source=source.replace("import init,* as runtime from './pkg/tilo_browser_runtime.js';","const init=async()=>{};const runtime=globalThis.__hostFixture.runtime;")
  .replace("import {acquireStorage} from './storage.mjs';","const acquireStorage=async()=>globalThis.__hostFixture.storage;")
  .replace(/from '(\.\/[^']+)'/g,(_all,path)=>`from '${new URL(path,import.meta.url).href}'`);
 const {mountDesktop}=await import(`data:text/javascript;base64,${Buffer.from(`${source}\n// ${Math.random()}`).toString('base64')}`);
 const target=new Node('div');document.body.append(target);const host=await mountDesktop(target,{app:app??(standalone?'writer':'desktop'),models,providerManagement});await tick();
 return {host,runtime,document,delivered,deliveredOwners,windowFocus,get ownershipReads(){return ownershipReads;},disposeRuntime(){allocated=false;},get state(){return state;},set state(value){state=value;},update:state=>update(JSON.stringify(state))};
}
test('model bootstrap stays mount scoped and forwards only its transport binding',async()=>{
 const models={endpoint:'https://models.example.test',sessionToken:'mount-one',profileId:'profile-one',modelId:'model-one',extra:'not a transport field'};
 const first=await fixture({models});
 assert.deepEqual(first.runtime.config.models,{endpoint:models.endpoint,sessionToken:'mount-one',profileId:'profile-one',modelId:'model-one'});
 await first.host.cleanup();
 const second=await fixture({models:{...models,sessionToken:'mount-two'}});
 assert.equal(second.runtime.config.models.sessionToken,'mount-two');
 await second.host.cleanup();
 const unbound=await fixture();
 assert.equal(unbound.runtime.config.models,undefined);
 await unbound.host.cleanup();
});

test('administration bootstrap is explicit, mount-local and separate from inference',async()=>{
 const models={endpoint:'https://models.example.test/model/rpc',sessionToken:'inference-only'};
 const providerManagement={endpoint:models.endpoint,sessionToken:'settings-one',apiKey:'must-not-forward',profileId:'not-needed'};
 const first=await fixture({app:'so.tilo.model-providers',models,providerManagement});
 assert.deepEqual(first.runtime.config.models,models,'omitting both selection fields uses the user default');
 assert.deepEqual(first.runtime.config.providerManagement,{endpoint:models.endpoint,sessionToken:'settings-one'});
 const second=await fixture({providerManagement:{endpoint:models.endpoint,sessionToken:'settings-two'}});
 assert.equal(second.runtime.config.app,'desktop','desktop control-panel tabs receive the same explicit grant');
 assert.equal(second.runtime.config.providerManagement.sessionToken,'settings-two');
 assert.equal(first.runtime.config.providerManagement.sessionToken,'settings-one','a second mount cannot replace the first session');
 const inferenceOnly=await fixture({models});
 assert.equal(inferenceOnly.runtime.config.providerManagement,undefined,'never reuse an inference token implicitly');
 const missingToken=await fixture({providerManagement:{endpoint:models.endpoint},models});
 assert.deepEqual(missingToken.runtime.config.providerManagement,{endpoint:models.endpoint},'a missing management token remains missing for Rust validation');
 await first.host.cleanup();await second.host.cleanup();await inferenceOnly.host.cleanup();await missingToken.host.cleanup();
});

test('standalone control-panel bootstraps preserve both exact application IDs',async()=>{
 for(const app of ['so.tilo.model-providers','so.tilo.settings']){
  const mounted=await fixture({app,providerManagement:{endpoint:'https://models.example.test/model/rpc',sessionToken:'admin-'+app}});
  assert.equal(mounted.runtime.config.app,app);
  assert.equal(mounted.runtime.config.providerManagement.sessionToken,'admin-'+app);
  assert.equal(mounted.runtime.config.models,undefined,'administration does not require an inference selection or token');
  await mounted.host.cleanup();
 }
});

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

test('capture source belongs to its mount and never takes input focus',async()=>{
 const f=await fixture({standalone:true});
 assert.ok(f.host.captureSource,'the shared host exposes a source, not a desktop screenshot');
 const calls=[];const ctx=new Proxy({}, {get:(_o,k)=>(...args)=>calls.push([k,...args])});
 const output={getContext:()=>ctx};
 const observer=createPanorama({canvas:output,sources:[{source:f.host.captureSource,rect:[0,0,100,50]}],width:100,height:50});
 const before=f.document.activeElement;
 assert.throws(()=>observer.render({cx:50,cy:25,scale:1}),/presented/);
 // A missing real backing size cannot silently yield a successful frame.
 f.update({phase:'presented',captureProtected:false,frames:2,record:'one',gpuGeneration:1,logicalSize:[100,50],surfaceSize:[100,50]});
 await tick();
 assert.throws(()=>observer.render({cx:50,cy:25,scale:1}),/dimensions/);
 assert.equal(f.document.activeElement,before);
 const originalCreate=f.document.createElement;
 f.document.createElement=tag=>{const n=originalCreate(tag);n.getContext=()=>ctx;return n;};
 f.host.canvas.width=100;f.host.canvas.height=50;
 f.update({phase:'presented',captureProtected:false,frames:3,record:'one',gpuGeneration:1,logicalSize:[100,50],surfaceSize:[100,50]});
 await tick();
 const receipt=observer.render({cx:50,cy:25,scale:1});
 assert.equal(receipt.panes[0].record,'one');assert.equal(receipt.panes[0].frame,3);
 assert.equal(receipt.panes[0].presenceAvailable,false);
 f.update({phase:'presented',captureProtected:false,frames:4,record:'one',gpuGeneration:1,logicalSize:[100,50],surfaceSize:[100,50]});
 assert.equal(observer.render({cx:50,cy:25,scale:1}).panes[0].frame,4,'pixels and generation are captured before queued page callbacks');
 f.update({clipboard:'copied'});assert.equal(observer.render({cx:50,cy:25,scale:1}).panes[0].frame,4);
 const draws=calls.filter(call=>call[0]==='drawImage').length;
 for(const captureProtected of [true,undefined]){
  f.update({phase:'presented',captureProtected,frames:5,record:'one',gpuGeneration:1,logicalSize:[100,50],surfaceSize:[100,50]});
  assert.throws(()=>observer.render({cx:50,cy:25,scale:1}),/capture protection/);
  assert.equal(calls.filter(call=>call[0]==='drawImage').length,draws,'protected or unknown pixels never reach either copy or output');
 }
 f.update({phase:'presented',captureProtected:false,frames:6,record:'one',gpuGeneration:1,logicalSize:[100,50],surfaceSize:[100,50]});
 assert.equal(observer.render({cx:50,cy:25,scale:1}).panes[0].frame,6);
 assert.equal(f.document.activeElement,before);observer.close();
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
test('the hidden IME editor paints nothing over the canvas',async()=>{
 const f=await fixture();
 const invisible=editor=>{for(const rule of ['background:transparent','color:transparent','caret-color:transparent','outline:none'])assert.ok(editor.style.cssText.includes(rule),`the IME editor sets ${rule}`);};
 invisible(f.host.editor);
 // A committed composition retires the editor for a copy of it.
 invisible(f.host.editor.cloneNode(false));
});

test('focus moving between the canvas and its IME editor is not a window blur',async()=>{
 const f=await fixture();f.host.editor.focus();f.windowFocus.length=0;f.delivered.length=0;
 // A press on the canvas takes focus from the editor; the next status hands it back.
 f.host.canvas.focus();f.update({phase:'presented',frames:2});await tick();
 assert.equal(f.document.activeElement,f.host.editor);
 assert.deepEqual(f.windowFocus.filter(type=>type==='blur'),[],'a pointer gesture on the canvas survives the IME handoff');
 assert.deepEqual(f.delivered,[['end',''],['start','']],'the IME still ends and restarts');
 const outside=new Node('input');f.document.body.append(outside);outside.focus();
 assert.deepEqual(f.windowFocus.at(-1),'blur','leaving the surface is still a window blur');
 f.host.canvas.focus();f.windowFocus.length=0;outside.focus();
 assert.deepEqual(f.windowFocus,['blur'],'and so is leaving it from the canvas');
});
test('current standalone and legacy runtimes retain local text input without desktop ownership',async()=>{
 for(const config of [{standalone:true},{legacy:true}]){
  const f=await fixture(config);f.host.editor.focus();f.host.editor.dispatchEvent(new DomEvent('beforeinput',{inputType:'insertText',data:'local'}));
  assert.deepEqual(f.delivered.filter(([kind])=>kind==='commit'),[['commit','local']]);assert.equal(f.ownershipReads,0);
 }
});
test('the focused field\'s purpose reaches the browser editor and free text keeps its defaults',async()=>{
 const f=await fixture({legacy:true});
 const hints=(inputmode,autocomplete,extra={})=>({inputmode,enterkeyhint:'',autocapitalize:'off',autocomplete,spellcheck:false,...extra});
 const field=(purpose,fieldHints)=>f.update({phase:'presented',frames:2,ime:{x:0,y:0,width:1,height:20,purpose,hints:fieldHints}});
 const editor=()=>f.host.editor;
 const attributes=()=>({inputmode:editor().inputMode,enterkeyhint:editor().enterKeyHint,autocapitalize:editor().autocapitalize,autocomplete:editor().autocomplete,spellcheck:editor().spellcheck});
 field('Phone',hints('tel','tel'));await tick();
 assert.deepEqual(attributes(),hints('tel','tel'));
 field('Search',hints('search','off',{enterkeyhint:'search'}));await tick();
 assert.deepEqual(attributes(),hints('search','off',{enterkeyhint:'search'}));
 field('Text',hints('text','off'));await tick();
 assert.deepEqual(attributes(),hints('text','off'),'free text: no capitalisation, autofill or browser spellcheck');
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
test('every wheel over the surface is the runtime\'s and page-unit wheels arrive as pixels on both axes',async()=>{
 const f=await fixture();const canvas=f.host.canvas;Object.assign(canvas,{clientWidth:800,clientHeight:600});
 const seen=[];canvas.addEventListener('wheel',event=>seen.push(event));
 const wheel=init=>{const event=new WheelEvent('wheel',{cancelable:true,...init});canvas.dispatchEvent(event);return event;};
 // Horizontal trackpad swipes (pixel) and mouse tilt wheels (line) pass
 // through untouched for the runtime; none may reach browser history.
 for(const init of [{deltaX:48,deltaY:1},{deltaX:-3,deltaMode:1},{deltaY:40,ctrlKey:true},{deltaX:-120}]){
  const event=wheel(init);assert.equal(event.defaultPrevented,true,JSON.stringify(init));
 }
 assert.equal(seen.length,4,'pixel and line wheels are not re-sent');
 seen.length=0;
 wheel({deltaX:0.5,deltaY:-1,deltaMode:2,clientX:10,clientY:20,altKey:true});
 assert.equal(seen.length,2,'a page-unit wheel is re-sent once, as pixels');
 const page=seen.find(event=>event.deltaMode===2),pixels=seen.find(event=>event.deltaMode===0);assert.equal(page.defaultPrevented,true);
 assert.deepEqual({deltaX:pixels.deltaX,deltaY:pixels.deltaY,deltaMode:pixels.deltaMode,clientX:pixels.clientX,clientY:pixels.clientY,altKey:pixels.altKey},
  {deltaX:400,deltaY:-600,deltaMode:0,clientX:10,clientY:20,altKey:true});
});
test('a Safari trackpad swipe reaches the runtime whole and never the page, from its first event',async()=>{
 const f=await fixture();const canvas=f.host.canvas;Object.assign(canvas,{clientWidth:800,clientHeight:600});
 const seen=[];canvas.addEventListener('wheel',event=>seen.push(event));
 const listener=canvas.listeners.get('wheel')[0];assert.equal(listener.options.passive,false,'a passive listener could not stop history swipes');
 // WebKit coalesces a swipe into fewer, larger pixel deltas, mirrors them
 // in legacy wheelDeltaX (-3x, inverted), and flags natural scrolling.
 const swipe=[1,3,9,22,40,38,30,21,14,9,6,4,2,1,1];
 const wheels=swipe.map(dx=>new WheelEvent('wheel',{cancelable:true,deltaX:dx,deltaY:0.25,wheelDeltaX:-3*dx,wheelDeltaY:0,webkitDirectionInvertedFromDevice:true}));
 for(const event of wheels){canvas.dispatchEvent(event);assert.equal(event.defaultPrevented,true,`deltaX ${event.deltaX}`);}
 assert.deepEqual(seen.map(event=>event.deltaX),swipe,'every delta reaches the runtime once, unscaled, in order');
});
test('the desktop search chord opens search from anywhere on the page and the browser never sees it',async()=>{
 const f=await fixture();const outside=new Node('input');f.document.body.append(outside);outside.focus();
 const canvasKeys=[];f.host.canvas.addEventListener('keydown',event=>canvasKeys.push(event.key));
 const chord=new DomEvent('keydown',{key:'k',code:'KeyK',metaKey:true});outside.dispatchEvent(chord);
 assert.equal(f.runtime.searchToggles,1);assert.equal(chord.defaultPrevented,true);
 assert.equal(f.document.activeElement,f.host.canvas,'search takes the keyboard');
 f.host.canvas.dispatchEvent(new DomEvent('keydown',{key:'k',code:'KeyK',metaKey:true}));
 assert.equal(f.runtime.searchToggles,2,'the same chord closes it');assert.deepEqual(canvasKeys,[],'Winit never sees the chord');
 const plain=new DomEvent('keydown',{key:'k',code:'KeyK',ctrlKey:true});outside.dispatchEvent(plain);
 assert.equal(f.runtime.searchToggles,2,'Ctrl+K stays the app\'s on a Mac');assert.equal(plain.defaultPrevented,false);
});
test('search opened from page chrome moves the keyboard to the surface',async()=>{
 const f=await fixture();const button=new Node('button');f.document.body.append(button);button.focus();
 f.update({phase:'presented',frames:2,searchOpen:true});await tick();assert.ok([f.host.canvas,f.host.editor].includes(f.document.activeElement),'the surface (or its text editor) has the keyboard');
 button.focus();f.update({phase:'presented',frames:3,searchOpen:true});await tick();assert.equal(f.document.activeElement,button,'only the opening frame moves focus');
});
test('desktop search chords follow the platform command key',async()=>{
 const {isDesktopSearchShortcut}=await import('./host-core.mjs');
 assert.equal(isDesktopSearchShortcut({key:'k',metaKey:true},true),true);
 assert.equal(isDesktopSearchShortcut({key:'K',ctrlKey:true},false),true);
 assert.equal(isDesktopSearchShortcut({key:'k',ctrlKey:true},true),false);
 assert.equal(isDesktopSearchShortcut({key:' ',code:'Space',metaKey:true},false),true);
 assert.equal(isDesktopSearchShortcut({key:'k',metaKey:true,shiftKey:true},true),false);
 assert.equal(isDesktopSearchShortcut({key:'k',metaKey:true,repeat:true},true),false);
 assert.equal(isDesktopSearchShortcut({key:'j',metaKey:true},true),false);
});


test('persistence flushes credential custody before publishing document writes',async()=>{
 const f=await fixture();const order=[];
 const storage=globalThis.__hostFixture.storage;
 storage.status={pendingWrites:1};
 storage.commit=async()=>{order.push('document');};
 storage.flush=async()=>{order.push('document-flush');};
 f.runtime.flush_files=async()=>{order.push('custody');throw Error('quota exceeded');};
 f.runtime.pending_writes=()=>[{revision:1n,key:'dependent-reference'}];
 await assert.rejects(f.host.flush(),/quota exceeded/);
 assert.deepEqual(order,['custody'],'failed custody must prevent document commit');
 order.length=0;f.runtime.flush_files=async()=>{order.push('custody');};
 await f.host.flush();
 assert.deepEqual(order,['custody','custody','document']);
 order.length=0;f.runtime.close=()=>[{revision:2n,key:'closing-reference'}];
 await f.host.cleanup();
 assert.deepEqual(order,['custody','custody','document'],'close must also persist custody first');
});
test('mounted semantic watch latches with pixels and preserves input through disposal',async()=>{
 const f=await fixture({standalone:true});const requests=[];f.runtime.close=()=>[];
 f.runtime.request_component_locus=(_id,target,observe)=>requests.push({target:JSON.parse(target),observe});
 const target={app:'writer',instance:1,record:'one',resource:'tilo-ui-bound:v1:host:lifetime:hex',action:'edit'};
 const before=f.document.activeElement,delivered=f.delivered.length;
 Object.assign(f.host.canvas,{width:100,height:50});
 const watch=f.host.observeSemanticTarget(target);assert.equal(watch.read(),null);
 const ctx=new Proxy({}, {get:()=>()=>{}});
 const observer=createPanorama({canvas:{getContext:()=>ctx},sources:[{source:f.host.captureSource,rect:[0,0,100,50]}],width:100,height:50});
 document.createElement=tag=>Object.assign(new Node(tag),{getContext:()=>ctx});
 const state={phase:'presented',captureProtected:false,presentation:true,frames:2,record:'one',gpuGeneration:1,layoutGeneration:1,logicalSize:[100,50],surfaceSize:[100,50],semanticLoci:[{target,rect:[2,3,20,30]}]};
 f.update(state);
 const semantic=watch.read(),pane=observer.render({cx:50,cy:25,scale:1}).panes[0];
 assert.deepEqual(semantic.rect,[2,3,20,30]);assert.equal(semantic.frame,pane.frame);assert.equal(semantic.gpuGeneration,pane.gpuGeneration);assert.equal(semantic.layoutGeneration,pane.layoutGeneration);assert.equal(semantic.presentedAtMs,pane.presentedAtMs);
 f.update({...state,presentation:false,semanticLoci:[]});assert.deepEqual(watch.read().rect,[2,3,20,30]);
 f.host.canvas.width=101;assert.equal(watch.read(),null);
 assert.equal(f.document.activeElement,before);assert.equal(f.delivered.length,delivered);
 watch.close();assert.equal(requests.at(-1).observe,false);assert.equal(watch.read(),null);
 observer.close();await f.host.cleanup();assert.throws(()=>f.host.observeSemanticTarget(target),/closed/);
});
test('desktop chords and text-editing keys typed in a focused text field reach the desktop, browser chords stay the browser\'s',async()=>{
 const f=await fixture();f.host.editor.focus();
 const canvasKeys=[];f.host.canvas.addEventListener('keydown',event=>canvasKeys.push([event.key,Boolean(event.metaKey),Boolean(event.ctrlKey),Boolean(event.altKey)]));
 // The fixture is a Mac: Super is Cmd, the stand-in Ctrl+Option.
 for(const init of [{key:'ArrowRight',code:'ArrowRight',metaKey:true},{key:'ArrowLeft',code:'ArrowLeft',ctrlKey:true,altKey:true},{key:'PageDown',code:'PageDown',metaKey:true},{key:'¡',code:'Digit1',ctrlKey:true,altKey:true}]){
  const event=new DomEvent('keydown',init);f.host.editor.dispatchEvent(event);
  assert.equal(event.defaultPrevented,true,`${init.code} is the desktop's, not the page's`);
  f.host.editor.dispatchEvent(new DomEvent('keyup',{key:init.key,code:init.code}));
 }
 assert.deepEqual(canvasKeys,[['ArrowRight',true,false,false],['ArrowLeft',false,true,true],['PageDown',true,false,false],['¡',false,true,true]]);
 canvasKeys.length=0;
 // Word movement and deletion edit the app's text field, so they reach it.
 for(const init of [{key:'ArrowLeft',code:'ArrowLeft',altKey:true},{key:'ArrowLeft',code:'ArrowLeft',altKey:true,shiftKey:true},{key:'Backspace',code:'Backspace',altKey:true}]){
  const event=new DomEvent('keydown',init);f.host.editor.dispatchEvent(event);
  assert.equal(event.defaultPrevented,true,`${JSON.stringify(init)} edits the app's text`);
  f.host.editor.dispatchEvent(new DomEvent('keyup',{key:init.key,code:init.code}));
 }
 assert.deepEqual(canvasKeys,[['ArrowLeft',false,false,true],['ArrowLeft',false,false,true],['Backspace',false,false,true]]);
 canvasKeys.length=0;
 // A browser chord stays the browser's.
 const address=new DomEvent('keydown',{key:'l',code:'KeyL',metaKey:true});f.host.editor.dispatchEvent(address);
 assert.equal(address.defaultPrevented,false,'Cmd+L stays native');
 assert.deepEqual(canvasKeys,[],'browser chords are not the app\'s');
});

const file=(name,type,bytes,size=bytes.length)=>({name,type,size,arrayBuffer:async()=>new Uint8Array(bytes).buffer});

test('a pasted picture is the first image file a paste carries, within the paste limit',()=>{
  const screenshot=file('image.png','image/png',[1,2]);
  assert.equal(pastedPicture({files:[file('notes.txt','text/plain',[1]),screenshot]}),screenshot);
  assert.equal(pastedPicture({files:[file('huge.png','image/png',[1],MAX_PASTED_PICTURE+1)]}),null);
  assert.equal(pastedPicture({files:[file('vector.svg','image/svg+xml',[1])]}),null,'only the types an app decodes');
  assert.equal(pastedPicture({}),null);
});

test('a drop hands over its files with their bytes, a bounded number of them',async()=>{
  assert.equal(draggingFiles({types:['text/plain','Files']}),true);
  assert.equal(draggingFiles({types:['text/plain']}),false,'dragged text is no file');
  const many=Array.from({length:MAX_DROPPED_FILES+3},(_,at)=>file(`f${at}.txt`,'text/plain',[at]));
  const dropped=await droppedFiles({files:many});
  assert.equal(dropped.length,MAX_DROPPED_FILES+1,'one more says the rest were not taken');
  assert.deepEqual(dropped[1],{name:'f1.txt',type:'text/plain',bytes:new Uint8Array([1])});
  assert.deepEqual(dropped.at(-1),{name:`f${MAX_DROPPED_FILES}.txt`,type:'text/plain',size:1,unread:'tooMany'});
});

test('a file too large to drop is named with its size, its contents never read',async()=>{
  const huge={name:'film.mov',type:'video/quicktime',size:MAX_DROPPED_FILE+1,arrayBuffer:async()=>{throw new Error('read');}};
  assert.deepEqual(await droppedFiles({files:[huge]}),[{name:'film.mov',type:'video/quicktime',size:MAX_DROPPED_FILE+1,unread:'tooLarge'}]);
});
