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
