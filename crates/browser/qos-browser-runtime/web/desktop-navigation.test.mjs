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
