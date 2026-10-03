import test from 'node:test';import assert from 'node:assert/strict';
import {storageRevision} from './storage.mjs';
test('revision ordering preserves decimal strings across digit boundaries and u64 precision',()=>{
 assert(storageRevision('10')>storageRevision('9'));
 assert(storageRevision('9007199254740993')>storageRevision('9007199254740992'));
 assert.equal(storageRevision(7),7n);assert.equal(storageRevision('18446744073709551615'),18446744073709551615n);
});
test('malformed and already rounded revisions fail before persistence',()=>{
 for(const value of [9007199254740992,-1,1.5,'01','-1','1e3','',null,{},'18446744073709551616'])assert.throws(()=>storageRevision(value),/revision/);
});
