import test from 'node:test';
import assert from 'node:assert/strict';
import {createDesktopLaunchFocus} from './desktop-launch-focus.mjs';
const route={workspace:'1',strip:'9007199254740993',group:'9007199254740994',tab:'9007199254740995'};
const pending={invocation:'launch',command:JSON.stringify({source:{room:'room'},command:{OpenTab:{app:'dev.qos.notes'}}})};
function fixture(){let generation='mount',epoch=0,time=0,entries=[];const selected=[];const f=createDesktopLaunchFocus({snapshot:()=>({generation,entries,workspaces:[{id:"1",room:"room"}]}),capture:()=>epoch,select:route=>selected.push(route),now:()=>time});return{f,selected,admit(){entries=[{route}];},navigate(){epoch++;},replace(){generation='replacement';},expire(){time=30001;}};}
const committed={invocation:'launch',room:'room',opened:{strip:route.strip,group:route.group,tab:route.tab}};
test('exact committed launch selects only after its canonical route is admitted',()=>{const x=fixture();x.f.pending(pending);x.f.committed(committed);assert.deepEqual(x.selected,[]);x.admit();x.f.refresh();assert.deepEqual(x.selected,[route]);x.f.committed(committed);assert.equal(x.selected.length,1);});
test('retained admission may win the race with the exact live commit notice and still selects once',()=>{const x=fixture();x.f.pending(pending);x.admit();x.f.committed(committed);x.f.committed(committed);assert.deepEqual(x.selected,[route]);});
test('old or unsolicited completion never activates a tab',()=>{for(const change of ['navigate','replace','expire']){const x=fixture();x.f.pending(pending);x.admit();x[change]();x.f.committed(committed);assert.deepEqual(x.selected,[]);}const x=fixture();x.admit();x.f.committed(committed);assert.deepEqual(x.selected,[]);});
test('wrong invocation room and exact route cannot steal focus',()=>{for(const bad of [{...committed,invocation:'other'},{...committed,room:'other'},{...committed,opened:{...committed.opened,tab:'9007199254740996'}}]){const x=fixture();x.f.pending(pending);x.admit();x.f.committed(bad);assert.deepEqual(x.selected,[]);}});
test('failure disposal and newer explicit navigation cancel launch selection',()=>{for(const cancel of [f=>f.updated({invocation:'launch',terminal:true,state:'failed'}),f=>f.close(),f=>f.cancel()]){const x=fixture();x.f.pending(pending);x.admit();cancel(x.f);x.f.committed(committed);assert.deepEqual(x.selected,[]);}});
test('retry callbacks cannot re-arm a cancelled or consumed launch',()=>{const x=fixture();x.f.pending(pending);x.f.cancel();x.f.pending(pending);x.admit();x.f.committed(committed);assert.deepEqual(x.selected,[]);});

test('exact tab identity is scoped to one workspace in the committed room',()=>{
 const selected=[];
 const wanted={...route,workspace:'wanted'},other={...route,workspace:'other'};
 let entries=[{route:other},{route:wanted}];
 const f=createDesktopLaunchFocus({snapshot:()=>({generation:'mount',entries,workspaces:[{id:'other',room:'other-room'},{id:'wanted',room:'room'}]}),select:r=>selected.push(r)});
 f.pending(pending);f.committed(committed);assert.deepEqual(selected,[wanted]);
});
