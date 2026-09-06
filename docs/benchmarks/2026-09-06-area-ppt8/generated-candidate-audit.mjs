// Read-only audit of generated source/metadata; not compiled-binary equivalence.
import fs from "node:fs";
import assert from "node:assert/strict";
import {inventory} from "./generated-shader-audit.mjs";

const before = JSON.parse(fs.readFileSync(new URL("generated-shader-before.json", import.meta.url), "utf8"));
const baseline = before.latest;
assert.ok(baseline?.shaders, "precompile baseline must exist");
const candidates = inventory().filter(item => item.has_ppt8);
assert.ok(candidates.length, "no generated PPT8 artifact exists");
const checked = [];
for (const candidate of candidates) {
  for (const [name, old] of Object.entries(baseline.shaders)) {
    assert.equal(candidate.shaders[name]?.normalized_tokens_sha256, old.normalized_tokens_sha256, `${candidate.path}: old ${name} tokens changed`);
    assert.equal(candidate.shaders[name]?.metadata_sha256, old.metadata_sha256, `${candidate.path}: old ${name} metadata changed`);
  }
  const source = fs.readFileSync(candidate.path, "utf8");
  const blocks = [...source.matchAll(/^        (\w+): ComputeShader \{\n([\s\S]*?)^        \},$/gm)];
  const field = (name, key) => {
    const block = blocks.find(match => match[1] === name);
    assert.ok(block, `missing ${name}`);
    const line = block[2].split("\n").find(value => new RegExp(`^${key}\\s*:`).test(value.trimStart()));
    assert.ok(line, `missing ${name}.${key}`);
    return line.trim();
  };
  for (const [old, added] of [["fine_area", "fine_area_ppt8"], ["fine_sparse_area", "fine_sparse_area_ppt8"]]) {
    assert.equal(field(added, "workgroup_size"), "workgroup_size: [2, 16, 1],");
    for (const key of ["bindings", "workgroup_buffers", "binding_indices"]) {
      assert.equal(field(added, key), field(old, key), `${candidate.path}: ${added}.${key} changed`);
    }
  }
  checked.push(candidate);
}
console.log(JSON.stringify({source_metadata_only: true, old_permutations_unchanged: 7, candidate_workgroup: [2, 16, 1], candidate_bindings_unchanged: true, checked}, null, 2));
