import fs from "node:fs";
import assert from "node:assert/strict";
import {inventory,shaders,normalized} from "./generated-shader-audit.mjs";
const baseline=JSON.parse(fs.readFileSync(new URL("generated-before.json",import.meta.url),"utf8")).latest;
const candidates=inventory().filter(x=>x.has_clip_single);
assert.ok(candidates.length,"no generated candidate exists");
const checked=[];
const marker="// Read input and compute Bic binary tree";
for(const c of candidates){
 for(const[name,old]of Object.entries(baseline.shaders)){
  assert.equal(c.shaders[name]?.normalized_tokens_sha256,old.normalized_tokens_sha256,c.path+": old "+name+" tokens changed");
  assert.equal(c.shaders[name]?.metadata_sha256,old.metadata_sha256,c.path+": old "+name+" metadata changed");
 }
 assert.deepEqual(Object.keys(c.shaders).filter(n=>!baseline.shaders[n]),["clip_leaf_single_group"],"unexpected added shader");
 const s=shaders(c.path),old=s.clip_leaf,added=s.clip_leaf_single_group;
 for(const k of ["workgroup_size","workgroup_buffers"])assert.equal(added.metadata[k],old.metadata[k],"candidate "+k+" changed");
 assert.equal(added.metadata.bindings,"bindings: Cow::Borrowed(&[Uniform, BufReadOnly, BufReadOnly, Buffer, Buffer]),","candidate used bindings changed");
 assert.equal(added.metadata.binding_indices,"binding_indices : Cow::Borrowed(&[0, 1, 2, 5, 6]),","candidate used binding indices changed");
 assert.ok(old.raw.includes(marker)&&added.raw.includes(marker),"missing preserved core marker");
 assert.equal(normalized(added.raw.slice(added.raw.indexOf(marker))),normalized(old.raw.slice(old.raw.indexOf(marker))),"intra-workgroup core changed");
 checked.push({path:c.path,old_shaders_preserved:Object.keys(baseline.shaders).length,new_shader:"clip_leaf_single_group",core_tokens_unchanged:true,old_metadata_unchanged:true,candidate_declared_host_binding_count:7,candidate_reflected_used_bindings:[0,1,2,5,6],workgroup_metadata_unchanged:true});
}
console.log(JSON.stringify({source_metadata_only:true,compiled_binary_equivalence_claimed:false,checked},null,2));
