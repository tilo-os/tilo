import test from 'node:test';
import assert from 'node:assert/strict';
import { MountRegistry, physicalViewport, wheelPixels, transactionComplete } from './host-core.mjs';
import * as hostCore from './host-core.mjs';

test('old cleanup and cancelled async mount cannot remove a newer generation', () => {
  const mounts = new MountRegistry();
  const target = {};
  const old = mounts.begin(target);
  const current = mounts.begin(target);
  assert.equal(mounts.current(old), false);
  assert.equal(mounts.finish(old, {close() { throw Error('stale mounted'); }}), false);
  assert.equal(mounts.remove(old), false);
  assert.equal(mounts.current(current), true);
  assert.equal(mounts.remove(current), true);
  assert.equal(mounts.current(current), false);
});
test('simultaneous mounts retain independent lifetime and terminal shutdown', () => {
  const mounts = new MountRegistry();
  const first = mounts.begin({}), second = mounts.begin({});
  assert.equal(mounts.remove(first), true);
  assert.equal(mounts.current(second), true);
  mounts.shutdown();
  assert.equal(mounts.current(second), false);
  assert.throws(() => mounts.begin({}), /shutdown/);
});
test('resize keeps logical geometry stable at DPR2 and suspends zero canvas', () => {
  assert.deepEqual(physicalViewport(600, 400, 2), {width:1200,height:800,dpr:2,suspended:false});
  assert.equal(physicalViewport(0,400,2).suspended, true);
  assert.deepEqual(wheelPixels(2,3,1,600,400), [-32,-48]);
  assert.deepEqual(wheelPixels(1,1,2,600,400), [-600,-400]);
});
test('a successful write request followed by transaction abort never acknowledges durability', async () => {
  const transaction = new EventTarget();
  const completion = transactionComplete(transaction);
  transaction.dispatchEvent(new Event('success'));
  transaction.dispatchEvent(new Event('abort'));
  await assert.rejects(completion, /aborted/);
});
test('only transaction completion acknowledges durability', async () => {
  const transaction = new EventTarget();
  let acknowledged = false;
  const completion = transactionComplete(transaction).then(() => { acknowledged=true; });
  transaction.dispatchEvent(new Event('success'));
  await Promise.resolve();
  assert.equal(acknowledged,false);
  transaction.dispatchEvent(new Event('complete'));
  await completion;
  assert.equal(acknowledged,true);
});

test('hidden editor gives printable text only to beforeinput and forwards editing keys once', () => {
  assert.equal(typeof hostCore.keyboardRoute,'function','the shared host owns one keyboard route');
  const route=hostCore.keyboardRoute;
  assert.equal(route({key:'é'},false,false),'text');
  assert.equal(route({key:'Dead'},false,false),'text','dead keys stay native so the browser can start composition');
  assert.equal(route({key:'Dead',altKey:true},false,true),'text','modifier-produced dead keys also stay native');
  assert.equal(route({key:'Backspace'},false,false),'forward');
  assert.equal(route({key:'Enter'},false,false),'forward');
  assert.equal(route({key:'Enter',isComposing:true},true,false),'composition');
});

test('Cmd and Ctrl app shortcuts forward while browser-reserved shortcuts remain native', () => {
  const route=hostCore.keyboardRoute;
  assert.equal(route({key:'a',metaKey:true},false,true),'forward');
  assert.equal(route({key:'z',metaKey:true,shiftKey:true},false,true),'forward');
  assert.equal(route({key:'a',ctrlKey:true},false,false),'forward');
  assert.equal(route({key:'v',metaKey:true},false,true),'clipboard');
  assert.equal(route({key:'r',metaKey:true},false,true),'browser');
  assert.equal(route({key:'l',ctrlKey:true},false,false),'browser');
});

test('modifier transitions include key release and blur reset', () => {
  const modifiers=hostCore.modifierSnapshot;
  assert.equal(typeof modifiers,'function');
  assert.deepEqual(modifiers({ctrlKey:true,altKey:false,shiftKey:true,metaKey:false}),[true,false,true,false]);
  assert.deepEqual(modifiers({}),[false,false,false,false]);
});

test('semantic native editors preserve native editing outside shared application keys', () => {
  const route=hostCore.semanticKeyboardRoute;
  assert.equal(typeof route,'function');
  assert.equal(route({key:'z',metaKey:true},false,true),'forward');
  assert.equal(route({key:'z',metaKey:true,shiftKey:true},false,true),'forward');
  assert.equal(route({key:'y',metaKey:true},false,true),'native');
  assert.equal(route({key:'y',metaKey:true,shiftKey:true},false,true),'native');
  assert.equal(route({key:'z',metaKey:true,ctrlKey:true},false,true),'native');
  assert.equal(route({key:'z',metaKey:true,altKey:true},false,true),'native');
  assert.equal(route({key:'z',ctrlKey:true},false,false),'forward');
  assert.equal(route({key:'z',ctrlKey:true,shiftKey:true},false,false),'forward');
  assert.equal(route({key:'y',ctrlKey:true},false,false),'forward');
  assert.equal(route({key:'y',ctrlKey:true,shiftKey:true},false,false),'native');
  assert.equal(route({key:'z',ctrlKey:true,metaKey:true},false,false),'native');
  assert.equal(route({key:'z',ctrlKey:true,altKey:true},false,false),'native');
  for(const event of [
    {key:'a',metaKey:true},
    {key:'c',metaKey:true},
    {key:'v',metaKey:true},
    {key:'x',metaKey:true},
    {key:'r',metaKey:true},
    {key:'Backspace'},
    {key:'é'},
  ])assert.equal(route(event,false,true),'native',event.key);
  assert.equal(route({key:'z',metaKey:true,isComposing:true},true,true),'native');
});

test('shared key forwarder sends one paired event and clears modifiers on blur', () => {
  const dispatched=[];const modifiers=[];let clears=0;
  const owner={};
  const forwarder=hostCore.createKeyboardForwarder({
    route:hostCore.semanticKeyboardRoute,
    dispatch:(type,event)=>dispatched.push([type,event.key,event.shiftKey]),
    publishModifiers:event=>modifiers.push(hostCore.modifierSnapshot(event)),
    clearModifiers:()=>{clears++;},
  });
  const down={key:'z',code:'KeyZ',metaKey:true,shiftKey:true,defaultPrevented:false,preventDefault(){this.defaultPrevented=true;}};
  const up={key:'z',code:'KeyZ',metaKey:false,shiftKey:false,defaultPrevented:false,preventDefault(){this.defaultPrevented=true;}};
  assert.equal(forwarder.keydown(owner,down,false,true),true);
  assert.equal(forwarder.keyup(owner,up),true);
  assert.equal(forwarder.keyup(owner,up),false,'a repeated keyup cannot dispatch twice');
  forwarder.blur(owner);
  assert.deepEqual(dispatched,[['keydown','z',true],['keyup','z',false]]);
  assert.deepEqual(modifiers,[[false,false,true,true],[false,false,false,false],[false,false,false,false]]);
  assert.equal(clears,1);
  assert.equal(down.defaultPrevented,true);
  assert.equal(up.defaultPrevented,true);
});

test('blur synthesizes release for its forwarded owner and stale owners cannot interfere', () => {
  const dispatched=[];const modifiers=[];let clears=0;
  const owner={},stale={};
  const forwarder=hostCore.createKeyboardForwarder({
    route:hostCore.semanticKeyboardRoute,
    mac:true,
    dispatch:(type,event)=>dispatched.push([type,event.key,event.metaKey,event.shiftKey]),
    publishModifiers:event=>modifiers.push(hostCore.modifierSnapshot(event)),
    clearModifiers:()=>{clears++;},
  });
  const down={key:'z',code:'KeyZ',metaKey:true,shiftKey:true,preventDefault(){}};
  const up={key:'z',code:'KeyZ',preventDefault(){this.defaultPrevented=true;}};
  assert.equal(forwarder.keydown(owner,down,false),true);
  assert.equal(forwarder.keyup(stale,up),false);
  assert.equal(forwarder.blur(stale),false);
  assert.equal(forwarder.blur(owner),true);
  assert.equal(forwarder.keyup(owner,up),false,'the later physical release is already settled');
  assert.deepEqual(dispatched,[
    ['keydown','z',true,true],
    ['keyup','z',false,false],
  ]);
  assert.deepEqual(modifiers,[[false,false,true,true]],'stale and post-blur releases cannot publish modifiers');
  assert.equal(clears,1);
  assert.equal(up.defaultPrevented,undefined,'an unowned keyup stays native');
});

test('beforeinput covers soft-keyboard edits without duplicating composition or paste', () => {
  const route=hostCore.beforeInputRoute;
  assert.equal(typeof route,'function');
  assert.deepEqual(route({inputType:'insertText',data:'é'},false),{kind:'commit',text:'é'});
  assert.deepEqual(route({inputType:'insertLineBreak'},false),{kind:'key',key:'Enter'});
  assert.deepEqual(route({inputType:'deleteContentBackward'},false),{kind:'key',key:'Backspace'});
  assert.deepEqual(route({inputType:'insertText',data:'x',isComposing:true},true),{kind:'ignore'});
  assert.deepEqual(route({inputType:'insertCompositionText',data:'東京'},false),{kind:'handled-elsewhere'});
  assert.deepEqual(route({inputType:'insertFromPaste'},false),{kind:'handled-elsewhere'});
  const limit=hostCore.MAX_TEXT_INSERT;
  assert.deepEqual(route({inputType:'insertText',data:'x'.repeat(limit)},false),{kind:'commit',text:'x'.repeat(limit)});
  assert.deepEqual(route({inputType:'insertText',data:'x'.repeat(limit+1)},false),{kind:'refuse',length:limit+1});
  assert.equal(hostCore.oversizedInsert('x'.repeat(limit+1)),true);
  assert.equal(hostCore.oversizedInsert(''),false);
});

test('focused application command K reaches the shared frame palette',()=>{
  assert.equal(hostCore.keyboardRoute({key:'k',ctrlKey:true},false,false),'forward');
  assert.equal(hostCore.keyboardRoute({key:'k',metaKey:true},false,true),'forward');
  assert.equal(hostCore.keyboardRoute({key:'k',ctrlKey:true,altKey:true},false,false),'text');
});

test('semantic Enter and Shift Enter reach the application while modified and composing Enter remain native',()=>{
  for(const mac of [false,true]){
    assert.equal(hostCore.semanticKeyboardRoute({key:'Enter'},false,mac),'forward');
    assert.equal(hostCore.semanticKeyboardRoute({key:'Enter',shiftKey:true},false,mac),'forward');
    for(const extra of [{ctrlKey:true},{altKey:true},{metaKey:true},{isComposing:true}])assert.equal(hostCore.semanticKeyboardRoute({key:'Enter',...extra},false,mac),'native');
  }
});

test('global shortcut accepts Option and Meta on macOS and other platforms only after open succeeds',()=>{
 assert.equal(typeof hostCore.routeGlobalSearchShortcut,'function');
 for(const mac of [true,false])for(const modifier of ['altKey','metaKey']){
  const events=[];const event={key:' ',code:'Space',[modifier]:true,preventDefault(){events.push('prevent');},stopImmediatePropagation(){events.push('stop');}};
  assert.equal(hostCore.routeGlobalSearchShortcut(event,false,()=>{events.push('open');},mac),true);
  assert.deepEqual(events,['open','prevent','stop']);
  for(const extra of [{repeat:true},{isComposing:true},{ctrlKey:true},{shiftKey:true},{altKey:true,metaKey:true},{defaultPrevented:true}]){
   events.length=0;assert.equal(hostCore.routeGlobalSearchShortcut({...event,...extra},false,()=>events.push('open'),mac),false);assert.deepEqual(events,[]);
  }
  events.length=0;assert.equal(hostCore.routeGlobalSearchShortcut(event,true,()=>events.push('open'),mac),false);
  assert.equal(hostCore.routeGlobalSearchShortcut(event,false,()=>{throw Error('blocked');},mac),false);assert.deepEqual(events,[]);
 }
 assert.equal(hostCore.overlayKeyboardRoute('SystemSearch',{key:'ArrowDown'},false,true),'forward');
 assert.equal(hostCore.overlayKeyboardRoute('SystemSearch',{key:'Enter'},false,true),'forward');
 assert.equal(hostCore.overlayKeyboardRoute('Document',{key:'ArrowDown'},false,true),'native');
 assert.equal(hostCore.overlayKeyboardRoute('Document',{key:'k',metaKey:true},false,true),'forward');
});

test('a status without semantics retains the last tree for keyboard routing while a tree replaces it',()=>{
  assert.equal(typeof hostCore.retainStatus,'function','the host retains status fields across partial statuses');
  assert.equal(typeof hostCore.retainedWidget,'function');
  const tree={keyboard_focus_owner:'system-search',root:{path:'system-search',widget:'SystemSearch',children:[]}};
  let state=hostCore.retainStatus(undefined,{phase:'presented',frames:1,semantics:tree});
  state=hostCore.retainStatus(state,{phase:'presented',frames:2});
  assert.strictEqual(state.semantics,tree,'an omitted tree keeps the retained one');
  assert.equal(state.frames,2,'other fields still update');
  assert.equal(hostCore.retainedWidget(state),'SystemSearch');
  assert.equal(hostCore.overlayKeyboardRoute(hostCore.retainedWidget(state),{key:'ArrowDown'},false,true),'forward','keyboard routing uses the retained tree');
  const replaced={root:{path:'desktop',widget:'Document',children:[]}};
  state=hostCore.retainStatus(state,{phase:'presented',frames:3,semantics:replaced});
  assert.strictEqual(state.semantics,replaced);
  assert.equal(hostCore.overlayKeyboardRoute(hostCore.retainedWidget(state),{key:'ArrowDown'},false,true),'native');
  assert.equal(hostCore.retainedWidget(undefined),undefined,'before the first status nothing is retained');
});

test('ownership invalidation forgets held keys without releasing into the next owner',()=>{
 const events=[];const keys=hostCore.createKeyboardForwarder({route:hostCore.keyboardRoute,dispatch:type=>events.push(type)});
 const owner={},event={key:'Enter',preventDefault(){}};
 keys.keydown(owner,event,false);assert.equal(typeof keys.invalidate,'function');keys.invalidate();
 keys.keyup(owner,event);keys.blur(owner);assert.deepEqual(events,['keydown']);
});

const ownership=(sequence='9007199254740993',activation='1',owner='writer',ime={x:1,y:2,height:20})=>({sequence,activation,owner,ime});
test('input capability rejects changed exact counters, inactive and replacement mounts',()=>{
 let value=ownership(),active=true,changes=0;
 assert.equal(typeof hostCore.createInputScope,'function');
 const scope=hostCore.createInputScope({read:()=>JSON.stringify(value),active:()=>active,onChange:()=>changes++});
 const first=scope.capture();assert.equal(scope.valid(first),true);
 value=ownership('9007199254740992');assert.equal(scope.valid(first),false);assert.equal(changes,1);
 const second=scope.capture();active=false;assert.equal(scope.valid(second),false);
});
test('only latest accepted activation hands off, using fresh IME and external focus epoch',async()=>{
 let value=ownership(),epoch=0,active=true;const focused=[];
 assert.equal(typeof hostCore.createInputScope,'function');assert.equal(typeof hostCore.createFocusHandoff,'function');
 const scope=hostCore.createInputScope({read:()=>JSON.stringify(value),active:()=>active});
 const handoff=hostCore.createFocusHandoff({scope,epoch:()=>epoch,focus:state=>focused.push(state.ime?'editor':'canvas')});
 let before=scope.capture();value=ownership('2','2');const first=handoff.complete(before,epoch);
 before=scope.capture();value=ownership('3','3','calculator',null);const second=handoff.complete(before,epoch);
 await Promise.all([first,second]);assert.deepEqual(focused,['canvas']);
 before=scope.capture();value=ownership('4','4');const outside=handoff.complete(before,epoch);epoch++;await outside;assert.deepEqual(focused,['canvas']);
 before=scope.capture();value=ownership('5','5');const pointer=handoff.complete(before,epoch);value=ownership('6','5');await pointer;assert.deepEqual(focused,['canvas']);
 before=scope.capture();value=ownership('7','6');const disposed=handoff.complete(before,epoch);active=false;await disposed;assert.deepEqual(focused,['canvas']);
});
test('canceled hidden composition ignores late end and input, then new composition works',()=>{
 let value=ownership();const delivered=[];
 assert.equal(typeof hostCore.createInputScope,'function');assert.equal(typeof hostCore.createCompositionSession,'function');
 const scope=hostCore.createInputScope({read:()=>JSON.stringify(value),active:()=>true});
 const session=hostCore.createCompositionSession({scope,send:(...args)=>delivered.push(args),clear:()=>{}});
 session.start();session.update('old');value=ownership('2','2','chat');
 session.end('old');assert.equal(session.blocks({inputType:'insertText',data:'old'}),true);
 assert.deepEqual(delivered,[['start',''],['preedit','old']]);
 session.start();session.update('new');session.end('new');assert.deepEqual(delivered.slice(2),[['start',''],['preedit','new'],['preedit',''],['commit','new']]);
});

test('suspension permanently revokes input captured before resume',()=>{
 let active=true;const scope=hostCore.createInputScope({read:()=>JSON.stringify(ownership()),active:()=>active});const before=scope.capture();
 active=false;assert.equal(scope.capture(),null);active=true;assert.equal(scope.valid(before),false);assert.equal(scope.valid(scope.capture()),true);
});
test('canceled composition stays revoked across tasks until a new session',async()=>{
 let value=ownership();const scope=hostCore.createInputScope({read:()=>JSON.stringify(value),active:()=>true});
 const session=hostCore.createCompositionSession({scope,send:()=>{},clear:()=>{}});session.start();value=ownership('2','2');session.end('old');
 assert.equal(session.blocks({inputType:'insertText',data:'old'}),true);await new Promise(resolve=>setTimeout(resolve,5));
 assert.equal(session.blocks({inputType:'insertText',data:'new'}),true);session.start();assert.equal(session.composing,true);
});
