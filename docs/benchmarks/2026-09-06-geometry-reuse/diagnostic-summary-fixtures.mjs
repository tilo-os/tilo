// Synthetic summary fixtures, not renderer runs.
import assert from "node:assert/strict";
import {summarize} from "./diagnostic-summary.mjs";
const plan=["default","bounds"].flatMap(variant=>[1,2,3].map(round=>({name:variant+round,variant,round,binary_sha256:"test"})));
const parsed={geometry:{
 counts:{values:{observations:1000,accepted:1000,hits:0,misses:1000,rank1:0,rank2:0,rank3:0,rank4:0}},
 bytes:{values:{extracted_total:16000,copied_payload:16000,retained_payload:64,peak_payload:64,descriptor_equal:0,layout_different:0,viewport_different:0,capacities_different:0,mode_different:0}},
 rejects:{values:{missing_or_malformed:0,empty:0,oversized:0,cpu_route:0,recovery_or_invalid_route:0,pending_bump_readback:0}}
}};
const runs=plan.map(spec=>({...spec,exit_code:1,output_truncated:false,parsed_result:{issues:[],parsed}}));
const passed=[];
function test(name,fn){fn();passed.push(name);}
test("six complete diagnostic reports aggregate by variant",()=>{const s=summarize(plan,runs);assert.equal(s.complete_diagnostic_reports,6);assert.equal(s.groups[0].observations,3000);assert.equal(s.groups[0].snapshot_copy_payload_bytes,48000);assert.equal(s.groups[1].peak_retained_payload_bytes,64);assert.equal(s.acceptance_eligible,false);assert.equal(s.performance_improvement_measured,false);});
test("failed attempt retained without replacement",()=>{const failed=structuredClone(runs);failed[0].exit_code=124;failed[0].parsed_result.issues=["timeout"];const s=summarize(plan,failed);assert.equal(s.attempted_runs,6);assert.equal(s.complete_diagnostic_reports,5);assert.equal(s.groups[0].complete,2);assert.equal(s.groups[0].observations,2000);assert.equal(s.groups[0].incomplete.length,1);});
test("truncated run cannot enter totals",()=>{const changed=structuredClone(runs);changed[0].output_truncated=true;assert.equal(summarize(plan,changed).complete_diagnostic_reports,5);});
test("missing attempt stays explicit",()=>{const s=summarize(plan,runs.slice(1));assert.deepEqual(s.missing,["default1"]);assert.equal(s.attempted_runs,5);});
test("duplicate attempt rejected",()=>assert.throws(()=>summarize(plan,[...runs,runs[0]]),/Duplicate/));
test("wrong binary identity rejected",()=>{const changed=structuredClone(runs);changed[0].binary_sha256="other";assert.throws(()=>summarize(plan,changed),/mismatched/);});
test("ordinary success exit is not diagnostic completion",()=>{const changed=structuredClone(runs);changed[0].exit_code=0;assert.equal(summarize(plan,changed).complete_diagnostic_reports,5);});
test("duplicate declaration rejected",()=>assert.throws(()=>summarize([...plan.slice(0,5),plan[0]],runs),/unique/));
test("missing parser result excluded",()=>{const changed=structuredClone(runs);delete changed[0].parsed_result;assert.equal(summarize(plan,changed).complete_diagnostic_reports,5);});
console.log(JSON.stringify({synthetic_fixtures:true,passed:passed.length,tests:passed},null,2));
