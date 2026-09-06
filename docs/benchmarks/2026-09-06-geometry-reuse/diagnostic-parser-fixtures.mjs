// Synthetic parser fixtures built around an existing ordinary log; NOT benchmark runs.
import assert from "node:assert/strict";
import {readFileSync} from "node:fs";
import {parse,audit} from "./diagnostic-parser.mjs";
const ordinary=readFileSync(new URL("../2026-09-06-clip-single-group/shell-default-round-1.log",import.meta.url),"utf8");
const base=ordinary
 .replace(/^gates:.*$/m,"gates: mean<=25000us p99<=50000us (DisabledDiagnostic)")
 .replace(/^Error:.*$/m,'Error: "diagnostic run completed: acceptance gates are intentionally disabled"');
const header="diagnostic: QOS_REDRAW_BENCH_PROFILE=1; phase timing is enabled and acceptance gates are disabled";
const timestamp="diagnostic GPU dispatch timestamps: matched=0/0 missing=0 stale=0 unsupported=0 disabled=1000 pending=0 map_failed=0 invalid=0 overflowed=0 no_submission_id=0 recovery_submissions=0; diagnostic_buffers=0B";
const counts="diagnostic geometry-input exact reuse: observations=1000 accepted=1000 misses=1000 hits=0 rank1=0 rank2=0 rank3=0 rank4=0 entries=4";
const bytes="  extracted_geometry_span_bytes(total=16000 min=16 max=16) copied_cpu_payload_bytes=16000 retained_cpu_payload_bytes=64 peak_cpu_payload_bytes=64 (payload only; descriptor and allocator overhead excluded); matching-span descriptors: equal=0 layout_different=0 viewport_different=0 capacities_different=0 full_or_sparse_mode_different=0";
const rejects="  history_invalidations(primary mutually-exclusive): missing_or_malformed=0 empty=0 oversized=0 cpu_route=0 recovery_or_invalid_route=0 pending_bump_readback=0; exact geometry inputs are opportunity evidence only, not safe GPU-cache entries, line-count provenance, or saved dispatches.";
const valid=[base,header,timestamp,counts,bytes,rejects,""].join("\n");
const result={exit_code:1,output_truncated:false};
const spec={variant:"default"};
const run=(s=valid,r=result)=>audit(parse(s),spec,r);
const passed=[];
function check(name, fn){fn();passed.push(name);}
check("complete no-hit diagnostic accepted as evidence only",()=>{assert.deepEqual(run(),[]);assert.equal(parse(valid).acceptance_eligible,false);assert.equal(parse(valid).stats.gate,"DISABLED_DIAGNOSTIC");assert.equal(parse(valid).exact_gate,null);});
const hit=valid
 .replace(counts,counts.replace("misses=1000 hits=0 rank1=0","misses=1 hits=999 rank1=999").replace("entries=4","entries=1"))
 .replace(bytes,bytes.replace("copied_cpu_payload_bytes=16000 retained_cpu_payload_bytes=64 peak_cpu_payload_bytes=64","copied_cpu_payload_bytes=16 retained_cpu_payload_bytes=16 peak_cpu_payload_bytes=16").replace("equal=0","equal=999"));
check("complete exact-hit diagnostic accepted without performance claim",()=>assert.deepEqual(run(hit),[]));
check("overlapping descriptor differences valid",()=>assert.deepEqual(run(hit.replace("equal=999 layout_different=0 viewport_different=0","equal=998 layout_different=1 viewport_different=1")),[]));
check("pending rejection remains counted",()=>assert.deepEqual(run(valid.replace("accepted=1000 misses=1000","accepted=999 misses=999").replace("copied_cpu_payload_bytes=16000","copied_cpu_payload_bytes=15984").replace("pending_bump_readback=0;","pending_bump_readback=1;")),[]));
check("missing geometry rejected",()=>assert.ok(run(valid.replace(counts,"")).length));
check("duplicate geometry rejected",()=>assert.ok(run(valid+"\n"+counts).length));
check("missing 1000-action summary rejected",()=>assert.ok(run(valid.replace(/DRM render\/readback at 3840x2160: samples=1000/,"DRM render/readback at 3840x2160: samples=999")).length));
check("timeouts rejected even with appended report",()=>assert.ok(run(valid,{...result,exit_code:124}).length));
check("truncated capture rejected",()=>assert.ok(run(valid,{...result,output_truncated:true}).length));
check("enabled acceptance gate rejected",()=>assert.ok(run(valid.replace("(DisabledDiagnostic)","(Pass)")).length));
check("rank sum mismatch rejected",()=>assert.ok(run(hit.replace("rank1=999","rank1=998")).length));
check("rejection accounting mismatch rejected",()=>assert.ok(run(valid.replace("pending_bump_readback=0;","pending_bump_readback=1;")).length));
check("payload cap rejected",()=>assert.ok(run(valid.replace("peak_cpu_payload_bytes=64","peak_cpu_payload_bytes=8388609")).length));
check("five-entry history rejected",()=>assert.ok(run(valid.replace("entries=4","entries=5")).length));
check("copy-without-miss accounting rejected",()=>assert.ok(run(hit.replace("copied_cpu_payload_bytes=16","copied_cpu_payload_bytes=2097153")).length));
check("descriptor equality mismatch rejected",()=>assert.ok(run(hit.replace("equal=999","equal=998")).length));
check("unsafe integer rejected",()=>assert.ok(run(valid.replace("observations=1000","observations=9007199254740992")).length));
check("timestamp instrumentation rejected",()=>assert.ok(run(valid.replace("disabled=1000 pending=0","disabled=999 pending=1")).length));
check("unexpected process errors rejected",()=>assert.ok(run(valid+"\nError: other").length));
check("wrong adapter rejected",()=>assert.ok(run(valid.replace('backend=Vulkan device_type=Cpu','backend=Gl device_type=Cpu')).length));
console.log(JSON.stringify({synthetic_fixtures:true,passed:passed.length,tests:passed},null,2));
