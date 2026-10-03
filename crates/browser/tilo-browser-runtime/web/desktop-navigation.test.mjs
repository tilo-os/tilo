import test from 'node:test';
import assert from 'node:assert/strict';
import {createDesktopNavigation} from './desktop-navigation.mjs';

test('exact identities, bounded publication and disposed/stale commands',async()=>{
 let raw={version:1,generation:'mount-1',active:{workspace:'18446744073709551615',strip:'2',group:'3',tab:'9007199254740993'},entries:[]},revision=1,calls=0;
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 nav.controller.subscribe(()=>calls++);nav.refresh();nav.refresh();assert.equal(calls,0);
 await nav.controller.dispatch({kind:'select',generation:raw.generation,route:raw.active});
 assert.equal(sent[0].route.tab,'9007199254740993');
 assert.equal(sent[0].route.workspace,'18446744073709551615');
 raw={...raw,active:{...raw.active,tab:'9007199254740995'}};revision=2;nav.refresh();assert.equal(calls,1);
 assert.equal(nav.controller.getSnapshot().active.tab,'9007199254740995');
 await assert.rejects(nav.controller.dispatch({kind:'select',generation:'old'}),/Stale/);
 nav.close();await assert.rejects(nav.controller.dispatch({kind:'select',generation:raw.generation}),/disposed/);
 assert.throws(()=>nav.controller.getSnapshot(),/disposed/);
});

test('a refused command rejects with the runtime reason and publishes nothing',async()=>{
 const raw={version:1,generation:'g',entries:[]};let calls=0;
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:()=>{throw Error('Global search unavailable');},active:()=>true});
 nav.controller.subscribe(()=>calls++);
 await assert.rejects(nav.controller.dispatch({kind:'global-search',generation:'g'}),/Global search unavailable/);
 assert.equal(calls,0);
});

test('pan, scroll, history and workspace activity reach the runtime verbatim',async()=>{
 const route={workspace:'18446744073709551615',strip:'9007199254740993',group:'3',tab:'4'};
 const raw={version:1,generation:'g',active:route,entries:[{route}],capabilities:{pan:true,scroll:true,history:true,workspaceTasks:true}};
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 const region=`${route.workspace}/${route.strip}/${route.group}/${route.tab}/primary`;
 await nav.controller.dispatch({kind:'pan',generation:'g',workspace:route.workspace,strip:route.strip,offset:240.5});
 await nav.controller.dispatch({kind:'scroll',generation:'g',region,action:{kind:'page',value:-1}});
 await nav.controller.dispatch({kind:'history',generation:'g',route,target:'back'});
 await nav.controller.dispatch({kind:'workspace-activity',generation:'g',workspace:route.workspace,view:'tasks'});
 await nav.controller.dispatch({kind:'close-workspace-activity',generation:'g',workspace:route.workspace});
 assert.deepEqual(sent.map(command=>command.kind),['pan','scroll','history','workspace-activity','close-workspace-activity']);
 assert.equal(sent[0].strip,'9007199254740993');assert.equal(sent[0].offset,240.5);
 assert.equal(sent[1].region,region);assert.deepEqual(sent[1].action,{kind:'page',value:-1});
 assert.deepEqual(sent[2].route,route);assert.equal(sent[2].target,'back');
 assert.equal(sent[3].view,'tasks');
});

test('a non-finite pan offset is refused before it reaches the runtime',async()=>{
 const raw={version:1,generation:'g',entries:[]};const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:command=>sent.push(command),active:()=>true});
 for(const offset of [NaN,Infinity,undefined,'12'])
  await assert.rejects(nav.controller.dispatch({kind:'pan',generation:'g',workspace:'1',strip:'2',offset}),/Invalid pan offset/);
 assert.equal(sent.length,0);
});

test('presentation-only changes (pan offset, back/forward, open view) republish the snapshot',async()=>{
 let raw={version:1,generation:'g',entries:[],desktopPan:{workspace:'1',strip:'2',offset:0,viewport:800,content:2000,max:1200},history:{canBack:false,canForward:false},workspaceActivity:null},revision=5;
 const seen=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:()=>{},active:()=>true});
 nav.controller.subscribe(snapshot=>seen.push(snapshot));
 raw={...raw,desktopPan:{...raw.desktopPan,offset:600}};revision=6;nav.refresh();
 raw={...raw,history:{canBack:true,canForward:false}};revision=7;nav.refresh();
 raw={...raw,workspaceActivity:{workspace:'1',view:'tasks',playing:false,coverage:'complete',tasks:[]}};revision=8;nav.refresh();
 nav.refresh();
 assert.equal(seen.length,3);
 assert.equal(seen[0].desktopPan.offset,600);
 assert.equal(seen[1].history.canBack,true);
 assert.equal(seen[2].workspaceActivity.view,'tasks');
 const refused=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:()=>{throw Error('Replay needs retained desktop history, which this desktop does not carry');},active:()=>true});
 await assert.rejects(refused.controller.dispatch({kind:'workspace-activity',generation:'g',workspace:'1',view:'replay'}),/Replay needs retained desktop history/);
});

test('close deduplicates and settles only when the route leaves the snapshot',async()=>{
 const route={workspace:'1',strip:'2',group:'3',tab:'4'};
 let raw={version:1,generation:'g',active:route,entries:[{route}]},revision=1;
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:command=>sent.push(command),active:()=>true});
 let settled=false;
 const first=nav.controller.dispatch({kind:'close',generation:'g',route}).then(()=>{settled=true;});
 const second=nav.controller.dispatch({kind:'close',generation:'g',route});
 assert.equal(sent.length,1,'repeat close must not dispatch twice');
 await Promise.resolve();assert.equal(settled,false);
 raw={...raw,active:null,entries:[]};revision=2;nav.refresh();
 await Promise.all([first,second]);assert.equal(settled,true);
 const pending=createDesktopNavigation({snapshot:()=>JSON.stringify({...raw,entries:[{route}]}),revision:()=>3,dispatch:()=>{},active:()=>true});
 const orphan=pending.controller.dispatch({kind:'close',generation:'g',route});
 pending.close();await assert.rejects(orphan,/disposed/);
});
test('open links the app before dispatching its full id',async()=>{
 const raw={version:1,generation:'surface',entries:[],capabilities:{open:true}},sent=[],installed=[];
 let release;const linked=new Promise(resolve=>{release=resolve;});
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>'1',dispatch:command=>{sent.push(JSON.parse(command));return'invocation';},awaitTerminal:async()=>{},
  install:async app=>{installed.push(app);await linked;return app==='writer'?'so.tilo.word-processor':app;},active:()=>true});
 assert.equal(nav.controller.getSnapshot().capabilities.open,true);
 const opening=nav.controller.dispatch({kind:'open',generation:'surface',app:'writer'});
 await Promise.resolve();
 assert.deepEqual(installed,['writer']);assert.deepEqual(sent,[],'nothing is opened before the app is linked');
 release();await opening;
 assert.deepEqual(sent,[{kind:'open',generation:'surface',app:'so.tilo.word-processor'}]);
 await assert.rejects(nav.controller.dispatch({kind:'open',generation:'old',app:'so.tilo.notes'}),/Stale/);
 await assert.rejects(nav.controller.dispatch({kind:'open',generation:'surface',app:''}),/Invalid application/);
 sent.length=0;
 await nav.controller.dispatch({kind:'open',generation:'surface',app:'so.tilo.ebook',record:'tab:00000000000000aa'});
 assert.deepEqual(sent,[{kind:'open',generation:'surface',app:'so.tilo.ebook',record:'tab:00000000000000aa'}],'an existing record travels with the open');
 for(const record of ['',7,null])await assert.rejects(nav.controller.dispatch({kind:'open',generation:'surface',app:'so.tilo.ebook',record}),/Invalid record/);
 nav.close();
 const unavailable=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>'1',dispatch:()=>{throw Error('dispatched');},awaitTerminal:async()=>{},install:async app=>{throw Error(`application "${app}" is not available`);},active:()=>true});
 await assert.rejects(unavailable.controller.dispatch({kind:'open',generation:'surface',app:'so.tilo.nope'}),/not available/);
 unavailable.close();
});


test('retained activity and notification commands preserve the runtime contract', async () => {
 const raw={version:1,generation:'g',entries:[],capabilities:{workspaceHistory:true,workspaceReplay:true,notifications:true},
  workspaceActivity:{workspace:'7',view:'replay',playing:false,coverage:'complete',inSurface:true},
  notifications:{active:0,retained:2,centerOpen:true}};
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 assert.equal(nav.controller.getSnapshot().workspaceActivity.inSurface,true);
 assert.equal(nav.controller.getSnapshot().notifications.retained,2);
 for(const view of ['catch-up','explore','replay'])
  await nav.controller.dispatch({kind:'workspace-activity',generation:'g',workspace:'7',view});
 await nav.controller.dispatch({kind:'notification-center',generation:'g',open:true});
 assert.deepEqual(sent.map(command=>command.kind),['workspace-activity','workspace-activity','workspace-activity','notification-center']);
 assert.deepEqual(sent.slice(0,3).map(command=>command.view),['catch-up','explore','replay']);
 assert.equal(sent[3].open,true);
 nav.close();
});
