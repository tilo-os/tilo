import test from 'node:test';
import assert from 'node:assert/strict';
import {createDesktopNavigation} from './desktop-navigation.mjs';
test('exact identities, bounded publication and disposed/stale commands', async()=>{
 let raw={version:1,generation:'18446744073709551615',active:{tab:'9007199254740993'}}, revision='1', sent=[], calls=0;
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 nav.controller.subscribe(()=>calls++);nav.refresh();nav.refresh();assert.equal(calls,0);
 await nav.controller.dispatch({kind:'select',generation:raw.generation,route:raw.active});
 assert.equal(sent[0].route.tab,'9007199254740993');
 raw={...raw,active:{tab:'9007199254740995'}};revision='2';nav.refresh();assert.equal(calls,1);
 await assert.rejects(nav.controller.dispatch({kind:'select',generation:'old'}),/Stale/);
 nav.close();await assert.rejects(nav.controller.dispatch({kind:'select',generation:raw.generation}),/disposed/);
 assert.throws(()=>nav.controller.getSnapshot(),/disposed/);
});
test('pan keeps real pixel metrics and exact mount/workspace/strip ownership across the bridge', async()=>{
 const raw={version:1,generation:'mount',desktopPan:{workspace:'18446744073709551615',strip:'9007199254740993',offset:12,viewport:600,content:1800,max:1200}};
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:value=>sent.push(JSON.parse(value)),active:()=>true});
 assert.deepEqual(nav.controller.getSnapshot().desktopPan,raw.desktopPan);
 const command={kind:'pan',generation:'mount',workspace:raw.desktopPan.workspace,strip:raw.desktopPan.strip,offset:1200};
 await nav.controller.dispatch(command);assert.deepEqual(sent,[command]);
 nav.close();await assert.rejects(nav.controller.dispatch(command),/disposed/);assert.equal(sent.length,1);
});
test('close is advertised only with terminal correlation, deduplicates, and waits for topology removal',async()=>{
 const route={workspace:'1',strip:'2',group:'3',tab:'9007199254740993'};
 let raw={version:1,generation:'surface',active:route,entries:[{route}],capabilities:{close:true}},revision='1',sent=[],terminalResolve;
 const terminal=new Promise(resolve=>{terminalResolve=resolve;});
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:command=>{sent.push(JSON.parse(command));return'invocation-1';},awaitTerminal:id=>{assert.equal(id,'invocation-1');return terminal;},active:()=>true});
 assert.equal(nav.controller.getSnapshot().capabilities.close,true);
 let settled=false;
 const first=nav.controller.dispatch({kind:'close',generation:'surface',route:raw.active}).then(()=>{settled=true;});
 const second=nav.controller.dispatch({kind:'close',generation:'surface',route:raw.active});
 assert.equal(sent.length,1,'repeat close must not dispatch twice');
 terminalResolve();await Promise.resolve();await Promise.resolve();assert.equal(settled,false,'terminal success alone is not visible removal');
 raw={...raw,active:null,entries:[]};revision='2';nav.refresh();
 await Promise.all([first,second]);assert.equal(settled,true);
 nav.close();
 const local=createDesktopNavigation({snapshot:()=>JSON.stringify({...raw,capabilities:{close:true}}),revision:()=>revision,dispatch:()=>{},active:()=>true});
 assert.equal(local.controller.getSnapshot().capabilities.close,false);
 await assert.rejects(local.controller.dispatch({kind:'close',generation:'surface',route:{workspace:'1',strip:'2',group:'3',tab:'4'}}),/unavailable/);
});
test('close rejects terminal failure and cleanup rejects unresolved unknown outcome',async()=>{
 const route={workspace:'1',strip:'2',group:'3',tab:'4'},raw={version:1,generation:'surface',active:route,entries:[{route}],capabilities:{close:true}};
 let rejectTerminal,attempt=0;
 const terminal=new Promise((_,reject)=>{rejectTerminal=reject;});
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>"1",dispatch:()=>`invocation-${++attempt}`,awaitTerminal:()=>attempt===1?terminal:new Promise(()=>{}),active:()=>true});
 const failed=nav.controller.dispatch({kind:'close',generation:'surface',route});rejectTerminal(Error('refused'));
 await assert.rejects(failed,/refused/);
 const pending=nav.controller.dispatch({kind:'close',generation:'surface',route});nav.close();
 await assert.rejects(pending,/disposed/);
});
test('close correlation survives route switches and disposes every remaining route',async()=>{
 const a={workspace:'1',strip:'2',group:'3',tab:'4'},b={workspace:'1',strip:'2',group:'3',tab:'5'},c={workspace:'1',strip:'2',group:'3',tab:'6'};
 let raw={version:1,generation:'surface',active:a,entries:[a,b,c].map(route=>({route})),capabilities:{close:true}},revision=1,next=0;
 const terminals=new Map(),sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>String(revision),dispatch:command=>{sent.push(JSON.parse(command));return`invocation-${++next}`;},awaitTerminal:invocation=>new Promise(resolve=>terminals.set(invocation,resolve)),active:()=>true});
 const closeA=nav.controller.dispatch({kind:'close',generation:'surface',route:a});
 raw={...raw,active:b};revision++;nav.refresh();
 let bSettled=false;const closeB=nav.controller.dispatch({kind:'close',generation:'surface',route:b}).then(()=>{bSettled=true;});
 assert.equal(sent.length,2);assert.equal(sent[0].route.tab,'4');assert.equal(sent[1].route.tab,'5');
 terminals.get('invocation-1')();raw={...raw,entries:[b,c].map(route=>({route}))};revision++;nav.refresh();await closeA;
 terminals.get('invocation-2')();await Promise.resolve();assert.equal(bSettled,false,'B waits for its own topology removal');
 raw={...raw,active:c,entries:[{route:c}]};revision++;nav.refresh();await closeB;assert.equal(bSettled,true);
 const closeC=nav.controller.dispatch({kind:'close',generation:'surface',route:c});nav.close();await assert.rejects(closeC,/disposed/);
});

test('global search has its own capability, opener, and accepted focus handoff', async()=>{
 let raw={generation:'live',capabilities:{globalSearch:true}},live=true,events=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>JSON.stringify(raw),active:()=>live,
  dispatch:()=>events.push('navigation'),search:(...args)=>events.push(['commands',...args]),
  globalSearch:(...args)=>{events.push(['global',...args]);if(args[1]==='blocked')throw Error('modal');},focusSearch:()=>events.push('focus')});
 await nav.controller.dispatch({kind:'global-search',generation:'live'});
 assert.deepEqual(events,[['global','live',''],'focus']);events=[];
 await nav.controller.dispatch({kind:'commands',generation:'live',query:'insert'});
 assert.deepEqual(events,[['commands','live','insert']]);events=[];
 await assert.rejects(nav.controller.dispatch({kind:'global-search',generation:'old'}),/Stale/);
 await assert.rejects(nav.controller.dispatch({kind:'global-search',generation:'live',query:'blocked'}),/modal/);
 assert.deepEqual(events,[['global','live','blocked']]);events=[];
 raw.capabilities.globalSearch=false;nav.refresh();
 await assert.rejects(nav.controller.dispatch({kind:'global-search',generation:'live'}),/unavailable/);
 live=false;await assert.rejects(nav.controller.dispatch({kind:'global-search',generation:'live'}),/unavailable/);
 live=true;nav.close();await assert.rejects(nav.controller.dispatch({kind:'global-search',generation:'live'}),/disposed/);
 assert.deepEqual(events,[]);
});

test('global capability requires a supplied opener and shortcut shares toolbar generation checks',async()=>{
 const missing=createDesktopNavigation({snapshot:()=>JSON.stringify({generation:'g',capabilities:{globalSearch:true}}),revision:()=>1,active:()=>true,dispatch(){throw Error('wrong path');}});
 assert.equal(missing.controller.getSnapshot().capabilities.globalSearch,false);
 await assert.rejects(missing.controller.dispatch({kind:'global-search',generation:'g'}),/unavailable/);
 let generation='one',calls=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify({generation,capabilities:{globalSearch:true}}),revision:()=>generation,active:()=>true,globalSearch:(...args)=>calls.push(args),focusSearch:()=>calls.push('focus')});
 nav.openGlobalSearch();assert.deepEqual(calls,[['one',''],'focus']);
 generation='two';assert.throws(()=>nav.openGlobalSearch('one'),/Stale/);
 nav.openGlobalSearch();assert.deepEqual(calls.at(-2),['two','']);
});
