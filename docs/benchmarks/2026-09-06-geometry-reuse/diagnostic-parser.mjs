// Pure parsing of diagnostic-only evidence. Never grants performance acceptance.
import {parse as parseBase} from "../2026-09-06-clip-single-group/comparison-parser.mjs";
import {audit as auditBase} from "../2026-09-06-xrgb-damage-output/comparison-parser.js";

function one(output, pattern, fields) {
 const matches=[...output.matchAll(pattern)];
 return {count:matches.length, values:matches.length===1?Object.fromEntries(fields.map((name,i)=>[name,Number(matches[0][i+1])])):null};
}
export function parse(output) {
 const p=parseBase(output,"shell");
 const counts=one(output,/^diagnostic geometry-input exact reuse: observations=(\d+) accepted=(\d+) misses=(\d+) hits=(\d+) rank1=(\d+) rank2=(\d+) rank3=(\d+) rank4=(\d+) entries=(\d+)$/gm,["observations","accepted","misses","hits","rank1","rank2","rank3","rank4","entries"]);
 const bytes=one(output,/^  extracted_geometry_span_bytes\(total=(\d+) min=(\d+) max=(\d+)\) copied_cpu_payload_bytes=(\d+) retained_cpu_payload_bytes=(\d+) peak_cpu_payload_bytes=(\d+) \(payload only; descriptor and allocator overhead excluded\); matching-span descriptors: equal=(\d+) layout_different=(\d+) viewport_different=(\d+) capacities_different=(\d+) full_or_sparse_mode_different=(\d+)$/gm,["extracted_total","extracted_min","extracted_max","copied_payload","retained_payload","peak_payload","descriptor_equal","layout_different","viewport_different","capacities_different","mode_different"]);
 const rejects=one(output,/^  history_invalidations\(primary mutually-exclusive\): missing_or_malformed=(\d+) empty=(\d+) oversized=(\d+) cpu_route=(\d+) recovery_or_invalid_route=(\d+) pending_bump_readback=(\d+); exact geometry inputs are opportunity evidence only, not safe GPU-cache entries, line-count provenance, or saved dispatches\.$/gm,["missing_or_malformed","empty","oversized","cpu_route","recovery_or_invalid_route","pending_bump_readback"]);
 const timestamp=one(output,/^diagnostic GPU dispatch timestamps: matched=(\d+)\/(\d+) missing=(\d+) stale=(\d+) unsupported=(\d+) disabled=(\d+) pending=(\d+) map_failed=(\d+) invalid=(\d+) overflowed=(\d+) no_submission_id=(\d+) recovery_submissions=(\d+); diagnostic_buffers=(\d+)B$/gm,["matched","requested","missing","stale","unsupported","disabled","pending","map_failed","invalid","overflowed","no_submission_id","recovery_submissions","buffers"]);
 const uniquePatterns={
  profile:/^diagnostic: QOS_REDRAW_BENCH_PROFILE=1; phase timing is enabled and acceptance gates are disabled$/gm,
  summary:/^DRM render\/readback at 3840x2160: samples=\d+ mean=\d+us p99=\d+us min=\d+us max=\d+us$/gm,
  exact_total:/^DRM timing total: total_us=\d+ samples=\d+$/gm,
  gate:/^gates: mean<=25000us p99<=50000us \(DisabledDiagnostic\)$/gm,
  attempts:/^Vello render attempts: total=\d+ sparse=\d+ full=\d+ recovery=\d+$/gm,
  completed:/^Error: "diagnostic run completed: acceptance gates are intentionally disabled"$/gm
 };
 return {...p, stats:p.stats?{...p.stats,gate:"DISABLED_DIAGNOSTIC"}:null, exact_gate:null,
  diagnostic:true, acceptance_eligible:false,
  geometry:{counts,bytes,rejects},timestamp,
  unique_counts:Object.fromEntries(Object.entries(uniquePatterns).map(([key,re])=>[key,[...output.matchAll(re)].length])),
  whole_scene_probe_present:/^diagnostic packed-scene exact reuse:/m.test(output),
  raw_error_lines:output.split("\n").filter(x=>x.startsWith("Error:")),
 };
}

export function audit(p,spec,result) {
 const issues=auditBase(p,{workload:"shell",variant:spec.variant});
 if(result.exit_code!==1||result.output_truncated)issues.push("incomplete or unexpected process result");
 for(const [name,n] of Object.entries(p.unique_counts))if(n!==1)issues.push("missing or duplicate "+name+" evidence");
 if(p.whole_scene_probe_present)issues.push("unplanned whole-scene probe");
 if(p.raw_error_lines.length!==1)issues.push("unexpected error report count");
 if(!Number.isSafeInteger(p.peak_rss_kib)||p.peak_rss_kib<=0)issues.push("invalid peak RSS");
 if(!p.stats||!Object.values(p.stats).filter(v=>typeof v==="number").every(n=>Number.isSafeInteger(n)&&n>0)||p.stats.min_us>p.stats.mean_us||p.stats.p99_us>p.stats.max_us)issues.push("invalid diagnostic timings");
 if(p.stats&&Math.floor(p.exact_mean_us)!==p.stats.mean_us)issues.push("rounded mean differs from exact total");
 for(const [name,v] of [["PPT8",p.ppt8],["clip-single",p.clip_single_group]]) {
  if(v.header_count!==1||v.counts_count!==1||v.env_requested!==false||v.renderer_requested!==false||v.available!==false||!v.counts_consistent||v.total!==0)issues.push("unplanned or missing "+name+" mode evidence");
 }
 const t=p.timestamp;
 if(t.count!==1||!t.values||Object.entries(t.values).some(([key,value])=>value!==(key==="disabled"?1000:0)))issues.push("GPU timestamp diagnostic was not wholly disabled");
 const g=p.geometry;
 for(const [name,group] of Object.entries(g)){
  if(group.count!==1||!group.values)issues.push("missing or duplicate geometry "+name);
  else if(!Object.values(group.values).every(n=>Number.isSafeInteger(n)&&n>=0))issues.push("invalid geometry "+name+" integer");
 }
 if(Object.values(g).every(x=>x.count===1&&x.values)){
  const c=g.counts.values,b=g.bytes.values,r=g.rejects.values;
  if(c.observations!==1000||c.accepted+Object.values(r).reduce((a,b)=>a+b,0)!==c.observations)issues.push("observation/rejection accounting mismatch");
  if(c.accepted!==c.hits+c.misses||c.hits!==c.rank1+c.rank2+c.rank3+c.rank4)issues.push("hit/rank accounting mismatch");
  if(c.entries>4||b.retained_payload>8*1024*1024||b.peak_payload>8*1024*1024||b.peak_payload<b.retained_payload||b.retained_payload>c.entries*2*1024*1024)issues.push("bounded history cap violated");
  if(b.copied_payload>b.extracted_total||b.copied_payload>c.misses*2*1024*1024||b.copied_payload<b.peak_payload||b.extracted_min>b.extracted_max)issues.push("payload byte accounting invalid");
  const diffs=[b.layout_different,b.viewport_different,b.capacities_different], unequal=c.hits-b.descriptor_equal;
  if(unequal<0||Math.max(...diffs)>unequal||diffs.reduce((a,b)=>a+b,0)<unequal||b.mode_different>c.hits)issues.push("descriptor equality accounting invalid");
 }
 return issues;
}
