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

test('semantic native editors forward only canonical history shortcuts', () => {
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
    {key:'Enter'},
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
});
