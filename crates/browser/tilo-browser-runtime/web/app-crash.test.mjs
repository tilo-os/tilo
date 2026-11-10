import test from 'node:test';
import assert from 'node:assert/strict';
import {createCrashQuarantine,createPageRestarts,crashAction,pageCrashAction,showPageCrash} from './app-crash.mjs';

function memoryStorage(){
  const values=new Map();
  return {getItem:key=>values.has(key)?values.get(key):null,setItem:(key,value)=>values.set(key,String(value))};
}

/** A document that only records what the crash screen put in it. */
function fakeDocument(){
  const make=tag=>({tag,children:[],attributes:{},textContent:'',
    setAttribute(name,value){this.attributes[name]=value;},
    append(...nodes){this.children.push(...nodes);},
    addEventListener(type,listener){this.bound={type,listener};},
    focus(){this.focused=true;}});
  const head=make('head'),body=make('body');
  return {head,body,createElement:make};
}

test('a crashed window is quarantined in its own workspace until it expires', () => {
  let clock=1000;
  const storage=memoryStorage();
  const crashes=createCrashQuarantine({storage,now:()=>clock});
  assert.equal(crashes.record('room-a',{tab:'18446744073709551615',message:'no filesystem on this platform'}),true);
  assert.deepEqual(crashes.entries('room-a'),[{tab:'18446744073709551615',message:'no filesystem on this platform'}]);
  assert.deepEqual(crashes.entries('room-b'),[]);
  // Another page of the same profile sees it.
  assert.equal(createCrashQuarantine({storage,now:()=>clock}).entries('room-a').length,1);
  clock+=8*24*60*60*1000;
  assert.deepEqual(crashes.entries('room-a'),[]);
});

test('a window that crashes again while quarantined does not restart the page', () => {
  const crashes=createCrashQuarantine({storage:memoryStorage(),now:()=>0});
  const report={tab:'7',message:'boom'};
  assert.equal(crashAction(report,crashes.record('room',report)),'restart');
  assert.equal(crashAction(report,crashes.record('room',report)),'report');
  crashes.forget('room','7');
  assert.deepEqual(crashes.entries('room'),[]);
});

test('a panic outside any window is only reported', () => {
  assert.equal(crashAction({tab:null,message:'boom'},true),'report');
  assert.equal(crashAction(undefined,true),'report');
});

test('unreadable or unavailable storage quarantines nothing and does not throw', () => {
  const broken={getItem:()=>'{not json',setItem:()=>{throw Error('quota');}};
  const crashes=createCrashQuarantine({storage:broken,now:()=>0});
  assert.deepEqual(crashes.entries('room'),[]);
  assert.equal(crashes.record('room',{tab:'1',message:'x'}),true);
  assert.deepEqual(createCrashQuarantine({storage:undefined}).entries('room'),[]);
});

test('a panic outside any window starts the page over once, then says so until the bound passes', () => {
  let clock=0;
  const restarts=createPageRestarts({storage:memoryStorage(),now:()=>clock});
  const did=[];
  const effects={restarts,reload:()=>did.push('reload'),show:message=>did.push(['screen',message]),reportError:()=>{}};
  assert.equal(pageCrashAction({tab:null,message:'a window was dropped twice'},effects),'reload');
  assert.deepEqual(did,['reload'],'the page starts itself over');
  // The load it just started crashes again: it would only come back into it.
  clock+=30_000;
  assert.equal(pageCrashAction({tab:null,message:'again'},effects),'screen');
  assert.deepEqual(did.at(-1),['screen','again'],'the person reads what happened and reloads when they choose');
  // Long enough after the restart to be another crash, not the same one.
  clock+=2*60*1000;
  assert.equal(pageCrashAction({tab:null,message:'later'},effects),'reload');
  assert.deepEqual(did.at(-1),'reload');
});

test('a panic outside any window is reported where it happened, not swallowed by the reload', () => {
  const said=[];
  let clock=0;
  const restarts=createPageRestarts({storage:memoryStorage(),now:()=>clock});
  const reportError=(kind,message)=>said.push({kind,message});
  pageCrashAction({tab:null,message:'a window was dropped twice'},{restarts,reload:()=>{},show:()=>{},reportError});
  clock+=30_000;
  pageCrashAction({tab:null,message:'a window was dropped twice'},{restarts,reload:()=>{},show:()=>{},reportError});
  assert.deepEqual(said,[
    {kind:'panic',message:'outside any window: a window was dropped twice'},
    {kind:'panic',message:'outside any window: a window was dropped twice'},
  ], 'both the restart and the screen report it, as one uncaught page error does');
});

test('a panic inside a window is still only that window\'s', () => {
  const storage=memoryStorage();
  const restarts=createPageRestarts({storage,now:()=>0});
  const report={tab:'7',message:'boom'};
  assert.equal(crashAction(report,true),'restart');
  assert.equal(crashAction(report,false),'report');
  restarts.claim();
  assert.equal(crashAction(report,true),'restart','the window crash is decided by its quarantine alone');
});

test('the crash screen says what happened and offers Reload, in the browser\'s theme', () => {
  const document=fakeDocument();
  let reloads=0;
  showPageCrash('a window was dropped twice',{document,reload:()=>reloads++});
  const screen=document.body.children[0];
  assert.equal(screen.attributes.role,'alert','a screen reader hears it as it appears');
  assert.equal(screen.className,'tilo-page-crash');
  const [title,said,button]=screen.children[0].children;
  assert.equal(title.textContent,'Tilo hit a problem');
  assert.equal(said.textContent,'a window was dropped twice');
  assert.equal(button.tag,'button');
  assert.equal(button.type,'button');
  assert.equal(button.attributes.type,undefined);
  assert.equal(button.textContent,'Reload');
  assert.equal(button.focused,true,'the button the person needs takes the focus');
  button.bound.listener();
  assert.equal(reloads,1);
  const css=document.head.children[0].textContent;
  assert.match(css,/prefers-color-scheme:light/,'the browser\'s light or dark, as the page\'s own screens');
  assert.match(css,/position:fixed/,'it covers the page it replaces');
});

test('a page with nowhere to put the screen reports no error of its own', () => {
  assert.equal(showPageCrash('boom',{document:{}}),undefined);
});
