// Entirely synthetic parser fixtures. No timings from these fixtures are evidence.
import assert from "node:assert/strict";
import fs from "node:fs";
import {parseRecords,stats,timestampCounts,auditReport,analyze} from "./analyze-capture.mjs";
const schema=JSON.parse(fs.readFileSync(new URL("./record-schema.json",import.meta.url),"utf8"));
const rows=Array.from({length:1000},(_,index)=>{
  const r={};
  for(const [type,keys] of Object.entries(schema.fields)) for(const key of keys)
    r[key]=type==="booleans"?false:type==="optional_numbers"?null:type==="strings"?"":0;
  Object.assign(r,{index,target:schema.actions[index%2],wall_us:index===999?100000:10000+index,
    submit_us:100,gpu_completion_map_us:1000,cpu_copy_swizzle_us:100,
    cpu_output_bytes:33177600,readback_bytes:1024,attempts:1,sparse_attempts:index?1:0,full_attempts:index?0:1,
    sparse_present:!!index,sparse_coarse_active_bins:!!index,sparse_coarse_adaptive:!!index,
    sparse_path_tile_bounds_applied:!!index,sparse_path_tile_bounds_tiles:index?1:0,
    timestamp_status:"ready",timestamp_status_submission_id:index+2,timestamp_expected_submission_id:index+2,
    timestamp_observed_submission_id:index+2,timestamp_exact_match:true,
    gpu_total_timed_stage_ns:600,gpu_coarse_ns:100,gpu_fine_ns:200,gpu_other_stage_ns:300,gpu_unknown_stage_ns:0,
    gpu_coarse_dispatches:1,gpu_fine_dispatches:1});
  return r;
});
function encode(input=rows) {
  return "diagnostic action correlation: records=1000 capacity=1000 (synthetic fixture)\n"+
    input.map(r=>"  action-correlation "+Object.entries(schema.fields).flatMap(([type,keys])=>keys.map(k=>
      k+"="+(type==="optional_numbers"?(r[k]===null?"None":"Some("+r[k]+")"):r[k]))).join(" ")).join("\n")+"\n";
}
const results=[];
function test(name,fn){fn();results.push({name,passed:true});}
test("all 1000 rows and alternating targets preserved",()=>assert.deepEqual(parseRecords(encode()),rows));
test("nearest rank keeps single worst outlier",()=>{
  const s=stats(rows.map(r=>r.wall_us));
  assert.equal(s.p99,10989);assert.equal(s.max,100000);assert.equal(s.count,1000);
  const a=analyze(rows);assert.equal(a.top_20_rows[0].index,999);
  assert.equal(a.by_target.topbar_tray_pill.count,500);assert.equal(a.slowest_10.count,10);
});
test("malformed or unsafe integers, duplicates and missing rows rejected",()=>{
  assert.throws(()=>parseRecords(encode().replace("wall_us=10000","wall_us=9007199254740993")),/unsafe/);
  assert.throws(()=>parseRecords(encode().replace("wall_us=10000","wall_us=1.2")),/integer/);
  assert.throws(()=>parseRecords(encode().replace("wall_us=10000","index=0")),/duplicate/);
  assert.throws(()=>parseRecords(encode(rows.slice(1))),/1000/);
});
test("stage partition and mismatched ids rejected",()=>{
  assert.throws(()=>parseRecords(encode().replace("gpu_total_timed_stage_ns=Some(600)","gpu_total_timed_stage_ns=Some(601)")),/partition/);
  assert.throws(()=>parseRecords(encode().replace("timestamp_expected_submission_id=Some(2)","timestamp_expected_submission_id=Some(3)")),/match flag/);
});
const missing=structuredClone(rows);
for(const [i,status] of ["pending","ready","disabled","unsupported","invalid","idle"].entries()){
  const r=missing[i];r.timestamp_exact_match=false;r.timestamp_observed_submission_id=null;
  for(const k of schema.fields.optional_numbers.filter(k=>k.startsWith("gpu_")))r[k]=null;
  r.timestamp_status=status;r.timestamp_status_submission_id=["disabled","unsupported","idle"].includes(status)?null:i+2;
  if(status==="ready"){r.timestamp_observed_submission_id=9999;r.timestamp_status_submission_id=9999;r.timestamp_stale=true;}
  if(status==="idle")r.timestamp_expected_submission_id=null;
  r.timestamp_missing=r.timestamp_expected_submission_id!==null && r.timestamp_observed_submission_id===null;
}
missing[0].attempts=2;missing[0].full_attempts=2;missing[0].recovery_attempts=1;
test("missing stale unsupported disabled idle and recovery remain explicit",()=>{
  const r=parseRecords(encode(missing));assert.deepEqual(r,missing);
  const c=timestampCounts(r);
  assert.equal(c.matched,994);assert.equal(c.expected,997);assert.equal(c.missing,3);assert.equal(c.stale,1);
  assert.equal(c.disabled,1);assert.equal(c.unsupported,1);assert.equal(c.no_submission_id,1);assert.equal(c.recovery_submissions,1);
  assert.equal(analyze(r).all.gpu_ns.gpu_fine_ns.count,994);
});
test("unmatched GPU samples cannot be zero filled",()=>{
  assert.throws(()=>parseRecords(encode(missing).replace("gpu_fine_ns=None","gpu_fine_ns=Some(0)")),/unmatched/);
});
const wall=stats(rows.map(r=>r.wall_us)),spec={variant:"bounds",record_artifact:"/synthetic.actions.log"};
let output=fs.readFileSync(new URL("../2026-09-06-xrgb-damage-output/shell-bounds-round-1.log",import.meta.url),"utf8")
  .replace(/^DRM render\/readback.*$/m,"DRM render/readback at 3840x2160: samples=1000 mean="+Math.floor(wall.mean)+"us p99="+wall.p99+"us min="+wall.min+"us max="+wall.max+"us")
  .replace(/^DRM timing total:.*$/m,"DRM timing total: total_us="+wall.total+" samples=1000")
  .replace(/^gates:.*$/m,"gates: mean<=25000us p99<=50000us (DisabledDiagnostic)")
  .replace(/native_bbox_tiles=\d+/,"native_bbox_tiles=999")
  .replace(/^Error:.*$/m,'Error: "diagnostic run completed: acceptance gates are intentionally disabled"');
output += "\ndiagnostic action correlation: records=1000 output_file=/synthetic.actions.log (synthetic)\n"+
"diagnostic phase means: input=0us registry=0us layout=0us scene=0us submit=100us gpu_completion_map=1000us settle=0us cpu_copy_swizzle=100us readback_bytes/sample=1024 cpu_output_bytes/sample=33177600\n"+
"diagnostic GPU dispatch timestamps: matched=1000/1000 missing=0 stale=0 unsupported=0 disabled=0 pending=0 map_failed=0 invalid=0 overflowed=0 no_submission_id=0 recovery_submissions=0; diagnostic_buffers=1024B\n";
test("all-row sums and quantiles match synthetic aggregate report",()=>assert.deepEqual(auditReport(output,rows,spec,1).issues,[]));
test("mismatched output total or phase mean fails audit",()=>{
  assert.throws(()=>auditReport(output.replace("total_us="+wall.total,"total_us=1"),rows,spec,1),/wall summary/);
  assert.throws(()=>auditReport(output.replace("submit=100us","submit=101us"),rows,spec,1),/phase mean/);
});
test("missing aggregate timestamp or duplicate wall evidence fails audit",()=>{
  assert.throws(()=>auditReport(output.replace("matched=1000/1000","matched=999/1000"),rows,spec,1),/timestamp aggregate/);
  assert.throws(()=>auditReport(output+"\nDRM timing total: total_us=1 samples=1000\n",rows,spec,1),/exact shell/);
});
test("diagnostics never become an acceptance pass or successful exit",()=>{
  assert.throws(()=>auditReport(output,rows,spec,0),/diagnostic completion/);
  assert.throws(()=>auditReport(output.replace("(DisabledDiagnostic)","(Pass)"),rows,spec,1),/gates not disabled/);
});
console.log(JSON.stringify({synthetic_only:true,passed:results.length,results},null,2));
