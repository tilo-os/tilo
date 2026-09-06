// Diagnostic evidence analysis only. Does not execute the benchmark or change gates.
import fs from "node:fs";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { parse as parseBase, audit as auditBase } from "../2026-09-06-xrgb-damage-output/comparison-parser.js";
const schema = JSON.parse(fs.readFileSync(new URL("./record-schema.json", import.meta.url), "utf8"));
const phases = ["input_us","registry_us","layout_us","scene_us","submit_us","gpu_completion_map_us","settle_us","cpu_copy_swizzle_us"];
const gpuFields = schema.fields.optional_numbers.filter(k => k.startsWith("gpu_"));
const statuses = ["disabled","unsupported","idle","pending","ready","map_failed","invalid","overflowed"];
const check = (ok, message) => { if (!ok) throw Error(message); };
const integer = s => { check(/^\d+$/.test(s), "invalid integer: "+s); const n=Number(s); check(Number.isSafeInteger(n), "unsafe integer: "+s); return n; };
const sum = values => values.reduce((a,b) => { check(Number.isSafeInteger(a+b), "unsafe sum"); return a+b; },0);
const count = (rows, fn) => rows.filter(fn).length;
function one(text, regex, label) { const matches=[...text.matchAll(regex)]; check(matches.length===1, label+": expected exactly one"); return matches[0]; }
export function parseRecords(text) {
  const lines=text.trimEnd().split(/\r?\n/);
  check(/^diagnostic action correlation: records=1000 capacity=1000 \(/.test(lines.shift()??""), "record header");
  check(lines.length===1000, "expected all 1000 action rows");
  const types = new Map(Object.entries(schema.fields).flatMap(([type, keys]) => keys.map(k=>[k,type])));
  return lines.map((line,index)=>{
    check(line.startsWith("  action-correlation "), "record prefix");
    const fields=line.slice("  action-correlation ".length).split(" ");
    check(fields.length===types.size, "field count");
    const row={};
    for(const field of fields) {
      const pair=/^([^=]+)=(.+)$/.exec(field); check(pair,"field syntax");
      const [,key,value]=pair;
      check(types.has(key) && !Object.hasOwn(row,key),"unknown/duplicate field: "+key);
      switch(types.get(key)) {
        case "numbers": row[key]=integer(value); break;
        case "booleans": check(value==="true"||value==="false","boolean"); row[key]=value==="true"; break;
        case "optional_numbers": {
          const m=/^Some\((\d+)\)$/.exec(value);
          check(value==="None"||m,"optional number"); row[key]=m?integer(m[1]):null; break;
        }
        default: row[key]=value;
      }
    }
    check(row.index===index && row.target===schema.actions[index%2],"index/target order");
    check(row.wall_us>0,"zero wall sample");
    check(row.attempts>=1 && row.attempts===row.sparse_attempts+row.full_attempts && row.attempts===1+row.recovery_attempts,"attempt accounting");
    const exp=row.timestamp_expected_submission_id, obs=row.timestamp_observed_submission_id;
    check(row.timestamp_exact_match===(exp!==null && obs!==null && exp===obs),"exact match flag");
    check(row.timestamp_missing===(exp!==null && obs===null),"missing flag");
    check(row.timestamp_stale===(exp!==null && obs!==null && exp!==obs),"stale flag");
    check(statuses.includes(row.timestamp_status),"timestamp status");
    const noStatusId=["disabled","unsupported","idle"].includes(row.timestamp_status);
    check((row.timestamp_status_submission_id===null)===noStatusId,"status id presence");
    if(obs!==null) check(row.timestamp_status==="ready" && row.timestamp_status_submission_id===obs,"decoded sample status");
    for(const k of gpuFields) check((row[k]!==null)===row.timestamp_exact_match,"unmatched GPU value: "+k);
    if(row.timestamp_exact_match) {
      check(row.gpu_total_timed_stage_ns===sum(["gpu_coarse_ns","gpu_fine_ns","gpu_other_stage_ns","gpu_unknown_stage_ns"].map(k=>row[k])),"stage partition");
      if(row.gpu_coarse_dispatches===0) check(row.gpu_coarse_ns===0,"coarse time without dispatch");
      if(row.gpu_fine_dispatches===0) check(row.gpu_fine_ns===0,"fine time without dispatch");
    }
    if(!row.sparse_present) {
      for(const [key,type] of types) if(key.startsWith("sparse_") && key!=="sparse_attempts")
        check(row[key]===(type==="booleans"?false:0),"absent sparse facts: "+key);
    }
    check(row.sparse_fine_quad_active_quads<=row.sparse_fine_quad_total_quads,"quad count bounds");
    check(sum(phases.map(k=>row[k]))<=row.wall_us,"phases exceed wall sample");
    return row;
  });
}
export function stats(values) {
  if(!values.length) return {count:0,mean:null,p99:null,min:null,max:null,total:0};
  const s=values.toSorted((a,b)=>a-b), total=sum(s);
  return {count:s.length,total,mean:total/s.length,p99:s[Math.ceil(.99*s.length)-1],min:s[0],max:s.at(-1)};
}
export function timestampCounts(rows) {
  const eligible=rows.filter(r=>!["disabled","unsupported"].includes(r.timestamp_status));
  const expected=eligible.filter(r=>r.timestamp_expected_submission_id!==null);
  return {
    matched:count(expected,r=>r.timestamp_exact_match), expected:expected.length,
    missing:count(expected,r=>!r.timestamp_exact_match), stale:count(expected,r=>r.timestamp_stale),
    unsupported:count(rows,r=>r.timestamp_status==="unsupported"),disabled:count(rows,r=>r.timestamp_status==="disabled"),
    pending:count(rows,r=>r.timestamp_status==="pending"),map_failed:count(rows,r=>r.timestamp_status==="map_failed"),
    invalid:count(rows,r=>r.timestamp_status==="invalid"),overflowed:count(rows,r=>r.timestamp_status==="overflowed"),
    no_submission_id:count(eligible,r=>r.timestamp_expected_submission_id===null),
    recovery_submissions:sum(rows.map(r=>r.recovery_attempts)),
  };
}
export function auditReport(output,rows,spec,exitCode) {
  const p=parseBase(output,"shell");
  const issues=auditBase(p,{...spec,workload:"shell"});
  check(issues.length===0,issues.join("; "));
  check(exitCode===1 && /^Error: "diagnostic run completed: acceptance gates are intentionally disabled"$/m.test(output),"not diagnostic completion");
  check(/gates:.*\(DisabledDiagnostic\)/.test(output),"diagnostic gates not disabled");
  const ack=one(output,/^diagnostic action correlation: records=(\d+) output_file=(\S+) \(/gm,"file acknowledgement");
  check(integer(ack[1])===1000 && ack[2]===spec.record_artifact,"record artifact acknowledgement mismatch");
  const wall=stats(rows.map(r=>r.wall_us));
  check(wall.total===p.exact_total_us && Math.floor(wall.mean)===p.stats.mean_us && wall.p99===p.stats.p99_us && wall.min===p.stats.min_us && wall.max===p.stats.max_us,"wall summary mismatch");
  const s=k=>sum(rows.map(r=>r[k]));
  for(const [key,field] of [["total","attempts"],["sparse","sparse_attempts"],["full","full_attempts"],["recovery","recovery_attempts"]])
    check(s(field)===p.attempts[key],"attempt aggregate mismatch: "+key);
  check(s("cpu_output_bytes")===p.xrgb.bytes,"CPU output bytes mismatch");
  const sparse=rows.filter(r=>r.sparse_present);
  check(sparse.length===p.sparse.total && count(sparse,r=>r.sparse_coarse_active_bins)===p.sparse.active &&
    count(sparse,r=>r.sparse_path_tile_bounds_applied)===p.sparse.applied &&
    sum(sparse.filter(r=>r.sparse_path_tile_bounds_applied).map(r=>r.sparse_path_tile_bounds_tiles))===p.sparse.bbox_tiles,"sparse aggregate mismatch");
  for(const row of rows) check(!row.sparse_fine_quad_skip_applied && !row.sparse_opaque_prune_selected && !row.sparse_solid_fuse_selected &&
    !row.sparse_segment_row_mask_selected && !row.sparse_canonical_segment_order_selected,"unplanned per-action experiment");
  const phase=one(output,/^diagnostic phase means: input=(\d+)us registry=(\d+)us layout=(\d+)us scene=(\d+)us submit=(\d+)us gpu_completion_map=(\d+)us settle=(\d+)us cpu_copy_swizzle=(\d+)us readback_bytes\/sample=(\d+) cpu_output_bytes\/sample=(\d+)$/gm,"phase means");
  [...phases,"readback_bytes","cpu_output_bytes"].forEach((k,i)=>check(Math.floor(s(k)/1000)===integer(phase[i+1]),"phase mean mismatch: "+k));
  const ts=one(output,/^diagnostic GPU dispatch timestamps: matched=(\d+)\/(\d+) missing=(\d+) stale=(\d+) unsupported=(\d+) disabled=(\d+) pending=(\d+) map_failed=(\d+) invalid=(\d+) overflowed=(\d+) no_submission_id=(\d+) recovery_submissions=(\d+); diagnostic_buffers=(\d+)B$/gm,"GPU timestamp totals");
  const counts=timestampCounts(rows);
  Object.values(counts).forEach((v,i)=>check(v===integer(ts[i+1]),"timestamp aggregate mismatch: "+Object.keys(counts)[i]));
  return {base:p,wall,timestamp_counts:counts,diagnostic_buffer_bytes:integer(ts[13]),issues:[]};
}
function correlation(rows,key) {
  const pairs=rows.filter(r=>r[key]!==null);
  if(pairs.length<2) return {count:pairs.length,pearson:null};
  const xm=pairs.reduce((s,r)=>s+r.wall_us,0)/pairs.length, ym=pairs.reduce((s,r)=>s+r[key],0)/pairs.length;
  let xy=0,xx=0,yy=0;
  for(const r of pairs) {const x=r.wall_us-xm,y=r[key]-ym; xy+=x*y;xx+=x*x;yy+=y*y;}
  return {count:pairs.length,pearson:xx&&yy?xy/Math.sqrt(xx*yy):null};
}
function group(rows) {
  return {
    count:rows.length,wall_us:stats(rows.map(r=>r.wall_us)),timestamp_counts:timestampCounts(rows),
    recovery_actions:count(rows,r=>r.recovery_attempts>0),
    phases_us:Object.fromEntries(phases.map(k=>[k,stats(rows.map(r=>r[k]))])),
    gpu_ns:Object.fromEntries(gpuFields.filter(k=>k.endsWith("_ns")).map(k=>[k,stats(rows.map(r=>r[k]).filter(v=>v!==null))])),
    wall_correlation:Object.fromEntries([...phases,...gpuFields.filter(k=>k.endsWith("_ns"))].map(k=>[k,correlation(rows,k)])),
    unassigned_wall_us:stats(rows.map(r=>r.wall_us-sum(phases.map(k=>r[k])))),
    cpu_output_bytes:sum(rows.map(r=>r.cpu_output_bytes)),readback_bytes:sum(rows.map(r=>r.readback_bytes))
  };
}
export function analyze(rows) {
  const slowest=rows.toSorted((a,b)=>b.wall_us-a.wall_us||a.index-b.index);
  return {all:group(rows),by_target:Object.fromEntries(schema.actions.map(t=>[t,group(rows.filter(r=>r.target===t))])),
    slowest_10:group(slowest.slice(0,10)),slowest_50:group(slowest.slice(0,50)),
    top_20_rows:slowest.slice(0,20),
    caveat:"Diagnostic-only. CPU phases are elapsed wall intervals; GPU stage times overlap them. GPU records cover only the exact final submission, not prior retries. Correlation is not causation. Unassigned wall time is not automatically scheduling time. Missing GPU rows retain their denominator."};
}
export function captureAnalysis(spec,exitCode) {
  const raw=fs.readFileSync(spec.record_artifact,"utf8"),out=fs.readFileSync(spec.log,"utf8");
  const rows=parseRecords(raw),audit=auditReport(out,rows,spec,exitCode);
  return {name:spec.name,variant:spec.variant,round:spec.round,acceptance_eligible:false,exit_code:exitCode,
    record_sha256:crypto.createHash("sha256").update(raw).digest("hex"),
    stdout_log_sha256:crypto.createHash("sha256").update(out).digest("hex"),audit,analysis:analyze(rows)};
}
if(process.argv[1]===fileURLToPath(import.meta.url)) {
  const plan=JSON.parse(fs.readFileSync(process.argv[2],"utf8"));
  const spec=plan.find(r=>r.name===process.argv[3]);
  check(spec,"unknown capture name");
  console.log(JSON.stringify(captureAnalysis(spec,Number(process.argv[4])),null,2));
}
