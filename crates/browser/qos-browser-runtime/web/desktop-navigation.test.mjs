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

test('workspace commands preserve exact wire values and wait for authoritative pane state', async()=>{
 const raw={generation:'mount-1',activeWorkspace:'9007199254740993',workspaces:[{id:'9007199254740993'}],capabilities:{workspaceSelect:true,workspaceHistory:true,workspaceReplay:true},workspaceActivity:null};
 const sent=[];
 const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:value=>sent.push(value),active:()=>true});
 for(const command of [
  {kind:'workspace',generation:'mount-1',workspace:'9007199254740993'},
  ...['catch-up','explore','replay','tasks'].map(view=>({kind:'workspace-activity',generation:'mount-1',workspace:'9007199254740993',view})),
  {kind:'close-workspace-activity',generation:'mount-1',workspace:'9007199254740993'},
 ]){await nav.controller.dispatch(command);assert.deepEqual(JSON.parse(sent.at(-1)),command);assert.equal(nav.controller.getSnapshot().workspaceActivity,null);}
 for(const kind of ['workspace','workspace-activity','close-workspace-activity']){
  const command={kind,generation:'mount-1',workspace:'9007199254740993',...(kind==='workspace-activity'?{view:'replay'}:{})};
  await assert.rejects(nav.controller.dispatch({...command,generation:'old'}),/Stale/);
  for(const workspace of [1,'01','-1','18446744073709551616','missing','2'])await assert.rejects(nav.controller.dispatch({...command,workspace}),/identity|unavailable/i);
 }
 nav.close();await assert.rejects(nav.controller.dispatch({kind:'workspace',generation:'mount-1',workspace:'9007199254740993'}),/disposed/);
});
test('workspace capabilities and active history owner are required',async()=>{
 for(const capabilities of [{},{workspaceSelect:true},{workspaceHistory:true,workspaceReplay:false}]){
  const raw={generation:'mount-1',activeWorkspace:'1',workspaces:[{id:'1'},{id:'2'}],capabilities};
  const nav=createDesktopNavigation({snapshot:()=>JSON.stringify(raw),revision:()=>1,dispatch:()=>assert.fail('unsupported command crossed runtime boundary'),active:()=>true});
  await assert.rejects(nav.controller.dispatch({kind:'workspace-activity',generation:'mount-1',workspace:'1',view:'replay'}),/unavailable/);
  await assert.rejects(nav.controller.dispatch({kind:'workspace-activity',generation:'mount-1',workspace:'2',view:'explore'}),/active|unavailable/);
 }
});

async function focusFixture(){
 const module=await import('./desktop-navigation-focus.mjs').catch(()=>({}));
 assert.equal(typeof module.createNavigationFocus,'function','mount focus handoff must exist');
 const document={activeElement:null};const events=[];
 const control=name=>({isConnected:true,visible:true,disabled:false,getClientRects(){return this.visible?[{}]:[];},focus(){document.activeElement=this;events.push(name);}});
 const opener=control('opener'),canvas=control('canvas');
 const surface={isConnected:true,inert:false,getClientRects:()=>[{}],contains:element=>element===canvas};
 let active=true;
 const focus=module.createNavigationFocus({document,surface,canvas,active:()=>active});
 const state={generation:'mount-1',activeWorkspace:'9007199254740993',workspaceActivity:null,workspaceActivityCloseReason:null};
 const command={kind:'workspace-activity',generation:'mount-1',workspace:state.activeWorkspace,view:'replay'};
 document.activeElement=opener;
 const open=()=>({...state,workspaceActivity:{workspace:state.activeWorkspace,view:'replay',playing:false,coverage:'complete'}});
 return {focus,state,command,open,events,opener,canvas,document,surface,disable:()=>{active=false;}};
}
test('external opener hands focus over only after authoritative open and restores on dismissal',async()=>{
 const f=await focusFixture();const request=f.focus.begin(f.command,f.state);f.focus.accepted(request);
 f.focus.update(f.state);assert.deepEqual(f.events,[]);
 f.focus.update(f.open());assert.deepEqual(f.events,['canvas']);
 f.focus.update(f.open());assert.deepEqual(f.events,['canvas']);
 f.focus.update({...f.state,workspaceActivityCloseReason:'dismissed'});assert.deepEqual(f.events,['canvas','opener']);
 f.focus.update({...f.state,workspaceActivityCloseReason:'dismissed'});assert.equal(f.events.length,2);
});
test('failed, stale, hidden and disposed external opens never move DOM focus',async()=>{
 for(const mode of ['failed','runtime-error','generation','workspace','hidden','disposed','inactive']){
  const f=await focusFixture();const request=f.focus.begin(f.command,f.state);
  if(mode==='failed')f.focus.failed(request);else f.focus.accepted(request);
  if(mode==='runtime-error')f.focus.failed();
  if(mode==='hidden')f.surface.inert=true;
  if(mode==='disposed')f.focus.close();
  if(mode==='inactive')f.disable();
  const snapshot=f.open();if(mode==='generation')snapshot.generation='mount-2';if(mode==='workspace')snapshot.activeWorkspace='other';
  f.focus.update(snapshot);assert.deepEqual(f.events,[],mode);
 }
});
test('resource navigation and document history never restore the external opener',async()=>{
 for(const reason of ['resource-opened','workspace-changed','access-changed','disposed',undefined]){
  const f=await focusFixture();f.focus.accepted(f.focus.begin(f.command,f.state));f.focus.update(f.open());
  f.focus.update({...f.state,workspaceActivityCloseReason:reason});assert.deepEqual(f.events,['canvas']);
 }
 const f=await focusFixture();f.focus.accepted(f.focus.begin(f.command,f.state));f.focus.update(f.open());
 f.document.activeElement=f.opener;
 f.focus.accepted(f.focus.begin({kind:'history',generation:'mount-1',route:{workspace:f.state.activeWorkspace}},f.open()));
 f.focus.update({...f.state,workspaceActivityCloseReason:'resource-opened'});
 assert.deepEqual(f.events,['canvas','canvas']);
 f.focus.update({...f.state,workspaceActivityCloseReason:'dismissed'});assert.equal(f.events.length,2);
});
test('background snapshots and disconnected opener do not steal focus',async()=>{
 const f=await focusFixture();f.focus.update(f.open());assert.deepEqual(f.events,[]);
 f.focus.accepted(f.focus.begin(f.command,f.state));f.focus.update(f.open());
 f.opener.isConnected=false;f.focus.update({...f.state,workspaceActivityCloseReason:'dismissed'});assert.deepEqual(f.events,['canvas']);
});

test('newer navigation cancels stale dismissal focus restoration',async()=>{
 for(const kind of ['workspace-activity','history','workspace','select','strip']){
  const f=await focusFixture();f.focus.accepted(f.focus.begin(f.command,f.state));f.focus.update(f.open());
  const next={...f.command,kind,view:'explore',route:{workspace:f.state.activeWorkspace}};
  f.focus.accepted(f.focus.begin(next,f.open()));
  f.focus.update({...f.state,workspaceActivityCloseReason:'dismissed'});
  assert.deepEqual(f.events,['canvas'],kind);
 }
});
test('dismissal respects a user who moved focus to another external control',async()=>{
 const f=await focusFixture();f.focus.accepted(f.focus.begin(f.command,f.state));f.focus.update(f.open());
 f.document.activeElement={};f.focus.update({...f.state,workspaceActivityCloseReason:'dismissed'});
 assert.deepEqual(f.events,['canvas']);
});

test('an explicit external close restores the original still-connected opener',async()=>{
 const f=await focusFixture();f.focus.accepted(f.focus.begin(f.command,f.state));f.focus.update(f.open());
 f.document.activeElement={isConnected:true,getClientRects:()=>[{}]};
 f.focus.accepted(f.focus.begin({kind:'close-workspace-activity',generation:'mount-1',workspace:f.state.activeWorkspace},f.open()));
 f.focus.update({...f.state,workspaceActivityCloseReason:'dismissed'});
 assert.deepEqual(f.events,['canvas','opener']);
});

test('retained desktop errors cancel only the pending request that encounters a new failure',async()=>{
 const f=await focusFixture();
 assert.equal(typeof f.focus.errors,'function','retained error channel needs edge tracking');
 f.focus.errors(['previous failure']);
 f.focus.accepted(f.focus.begin(f.command,f.state));
 f.focus.errors(['previous failure']);f.focus.update(f.open());assert.deepEqual(f.events,['canvas']);
 const g=await focusFixture();g.focus.accepted(g.focus.begin(g.command,g.state));
 g.focus.errors(['new failure']);g.focus.update(g.open());assert.deepEqual(g.events,[]);
});
