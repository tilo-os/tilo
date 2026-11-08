import test from 'node:test';
import assert from 'node:assert/strict';
import {createDesktopNavigation,verb} from './desktop-navigation.mjs';

test('exact identities, bounded publication and disposed verbs',async()=>{
 let raw={version:1,generation:'mount-1',active:{workspace:'18446744073709551615',strip:'2',group:'3',tab:'9007199254740993'},entries:[]},revision=1,calls=0;
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 nav.controller.subscribe(()=>calls++);nav.refresh();nav.refresh();assert.equal(calls,0);
 await nav.controller.dispatch({effect:'focus',args:{tab:raw.active.tab}});
 assert.deepEqual(sent[0],{effect:'focus',args:{tab:'9007199254740993'}});
 raw={...raw,active:{...raw.active,tab:'9007199254740995'}};revision=2;nav.refresh();assert.equal(calls,1);
 assert.equal(nav.controller.getSnapshot().active.tab,'9007199254740995');
 nav.close();await assert.rejects(nav.controller.dispatch({effect:'focus',args:{tab:raw.active.tab}}),/disposed/);
 assert.throws(()=>nav.controller.getSnapshot(),/disposed/);
});

test('a refused verb rejects with the runtime reason and publishes nothing',async()=>{
 const raw={version:1,generation:'g',entries:[]};let calls=0;
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:()=>{throw Error('Global search unavailable');},active:()=>true});
 nav.controller.subscribe(()=>calls++);
 await assert.rejects(nav.controller.dispatch({effect:'search'}),/Global search unavailable/);
 assert.equal(calls,0);
});

test('pan, scroll, history and workspace activity reach the runtime verbatim',async()=>{
 const route={workspace:'18446744073709551615',strip:'9007199254740993',group:'3',tab:'4'};
 const raw={version:1,generation:'g',active:route,entries:[{route}],capabilities:{pan:true,scroll:true,history:true,workspaceTasks:true}};
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 const region=`${route.workspace}/${route.strip}/${route.group}/${route.tab}/primary`;
 await nav.controller.dispatch({effect:'pan',args:{strip:route.strip,offset:240.5}});
 await nav.controller.dispatch({effect:'scroll',args:{region,action:{kind:'page',value:-1}}});
 await nav.controller.dispatch({effect:'history',args:{tab:route.tab,target:'back'}});
 await nav.controller.dispatch({effect:'activity',args:{workspace:route.workspace,view:'catch-up'}});
 await nav.controller.dispatch({effect:'close-activity',args:{workspace:route.workspace}});
 assert.deepEqual(sent.map(verb=>verb.effect),['pan','scroll','history','activity','close-activity']);
 assert.equal(sent[0].args.strip,'9007199254740993');assert.equal(sent[0].args.offset,240.5);
 assert.equal(sent[1].args.region,region);assert.deepEqual(sent[1].args.action,{kind:'page',value:-1});
 assert.equal(sent[2].args.tab,'4');assert.equal(sent[2].args.target,'back');
 assert.equal(sent[3].args.view,'catch-up');
});

test('a non-finite pan offset is refused before it reaches the runtime',async()=>{
 const raw={version:1,generation:'g',entries:[]};const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:command=>sent.push(command),active:()=>true});
 for(const offset of [NaN,Infinity,undefined,'12'])
  await assert.rejects(nav.controller.dispatch({effect:'pan',args:{strip:'2',offset}}),/Invalid pan offset/);
 assert.equal(sent.length,0);
});

test('presentation-only changes (pan offset, back/forward, banners) republish the snapshot',async()=>{
 let raw={version:1,generation:'g',entries:[],desktopPan:{workspace:'1',strip:'2',offset:0,viewport:800,content:2000,max:1200},history:{canBack:false,canForward:false}},revision=5;
 const seen=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:()=>{},active:()=>true});
 nav.controller.subscribe(snapshot=>seen.push(snapshot));
 raw={...raw,desktopPan:{...raw.desktopPan,offset:600}};revision=6;nav.refresh();
 raw={...raw,history:{canBack:true,canForward:false}};revision=7;nav.refresh();
 raw={...raw,notifications:{active:1,retained:0,centerOpen:false}};revision=8;nav.refresh();
 nav.refresh();
 assert.equal(seen.length,3);
 assert.equal(seen[0].desktopPan.offset,600);
 assert.equal(seen[1].history.canBack,true);
 assert.equal(seen[2].notifications.active,1);
 const refused=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:()=>{throw Error('Replay needs retained desktop history, which this desktop does not carry');},active:()=>true});
 await assert.rejects(refused.controller.dispatch({effect:'activity',args:{workspace:'1',view:'replay'}}),/Replay needs retained desktop history/);
});

test('close deduplicates and settles only when the tab leaves the snapshot',async()=>{
 const route={workspace:'1',strip:'2',group:'3',tab:'4'};
 let raw={version:1,generation:'g',active:route,entries:[{route}]},revision=1;
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:command=>sent.push(command),active:()=>true});
 let settled=false;
 const first=nav.controller.dispatch({effect:'close-tab',args:{tab:route.tab}}).then(()=>{settled=true;});
 const second=nav.controller.dispatch({effect:'close-tab',args:{tab:route.tab}});
 assert.equal(sent.length,1,'repeat close must not dispatch twice');
 await Promise.resolve();assert.equal(settled,false);
 raw={...raw,active:null,entries:[]};revision=2;nav.refresh();
 await Promise.all([first,second]);assert.equal(settled,true);
 const pending=createDesktopNavigation({snapshot:()=>JSON.stringify({...raw,entries:[{route}]}),revision:()=>3,dispatch:()=>{},active:()=>true});
 const orphan=pending.controller.dispatch({effect:'close-tab',args:{tab:route.tab}});
 pending.close();await assert.rejects(orphan,/disposed/);
});
test('launch is handed to the runtime as given',async()=>{
 const raw={version:1,generation:'surface',entries:[],capabilities:{launch:true}},sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>'1',dispatch:command=>{sent.push(JSON.parse(command));},active:()=>true});
 await nav.controller.dispatch({effect:'open-app',args:{app:'so.tilo.notes'}});
 assert.deepEqual(sent,[{effect:'open-app',args:{app:'so.tilo.notes'}}]);
 nav.close();
});


test('retained activity and notification verbs preserve the runtime contract', async () => {
 const raw={version:1,generation:'g',entries:[],capabilities:{workspaceHistory:true,workspaceReplay:true,notifications:true},
  notifications:{active:0,retained:2,centerOpen:true}};
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 assert.equal(nav.controller.getSnapshot().notifications.retained,2);
 for(const view of ['catch-up','explore','replay'])
  await nav.controller.dispatch({effect:'activity',args:{workspace:'7',view}});
 await nav.controller.dispatch({effect:'notifications',args:{open:true}});
 assert.deepEqual(sent.map(verb=>verb.effect),['activity','activity','activity','notifications']);
 assert.deepEqual(sent.slice(0,3).map(verb=>verb.args.view),['catch-up','explore','replay']);
 assert.equal(sent[3].args.open,true);
 nav.close();
});

test('the verbs a script builds are the ones the runtime takes',async()=>{
 const route={workspace:'1',strip:'2',group:'3',tab:'4'};
 const raw={version:1,generation:'g',active:route,entries:[{route}]};
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:command=>sent.push(JSON.parse(command)),active:()=>true});
 await nav.controller.dispatch(verb.openApp('so.tilo.notes'));
 await nav.controller.dispatch(verb.focus(route));
 await nav.controller.dispatch(verb.activity('1','explore'));
 await nav.controller.dispatch(verb.closeActivity('1'));
 const closing=nav.controller.dispatch(verb.closeTab(route));
 assert.deepEqual(sent,[{effect:'open-app',args:{app:'so.tilo.notes'}},{effect:'focus',args:{tab:'4'}},
  {effect:'activity',args:{workspace:'1',view:'explore'}},{effect:'close-activity',args:{workspace:'1'}},
  {effect:'close-tab',args:{tab:'4'}}]);
 nav.close();await assert.rejects(closing,/disposed/);
});

test('a snapshot read sees a move the surface made between reports',()=>{
 let raw={version:1,generation:'g',active:{tab:'1'},entries:[]},revision=1,calls=0;
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>revision,dispatch:()=>{},active:()=>true});
 nav.controller.subscribe(()=>calls++);
 raw={...raw,active:{tab:'2'}};revision=2;
 assert.equal(nav.controller.getSnapshot().active.tab,'2');
 assert.equal(calls,1);
 nav.controller.getSnapshot();
 assert.equal(calls,1);
});
