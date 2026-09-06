// Aggregation only: diagnostic geometry-input opportunity, never performance acceptance.
export function summarize(plan,runs) {
 if(plan.length!==6||new Set(plan.map(x=>x.name)).size!==6)throw Error("Expected six unique declared probes");
 if(new Set(runs.map(x=>x.name)).size!==runs.length)throw Error("Duplicate attempted probe");
 const byName=new Map(plan.map(x=>[x.name,x]));
 for(const run of runs){
  const spec=byName.get(run.name);
  if(!spec||run.variant!==spec.variant||run.round!==spec.round||run.binary_sha256!==spec.binary_sha256)throw Error("Unplanned or mismatched probe");
 }
 const complete=run=>run.exit_code===1&&!run.output_truncated&&run.parsed_result?.issues?.length===0;
 const groups=["default","bounds"].map(variant=>{
  const declared=plan.filter(p=>p.variant===variant), attempts=runs.filter(r=>r.variant===variant), ok=attempts.filter(complete);
  if(declared.length!==3||new Set(declared.map(p=>p.round)).size!==3)throw Error("Expected three declared rounds per variant");
  const sum=(group,key)=>ok.reduce((n,r)=>n+r.parsed_result.parsed.geometry[group].values[key],0);
  const max=(group,key)=>ok.length?Math.max(...ok.map(r=>r.parsed_result.parsed.geometry[group].values[key])):null;
  return {variant,declared:3,attempted:attempts.length,complete:ok.length,
   observations:sum("counts","observations"),accepted:sum("counts","accepted"),hits:sum("counts","hits"),misses:sum("counts","misses"),
   rank_hits:[1,2,3,4].map(n=>sum("counts","rank"+n)),
   matching_span_descriptor_equal:sum("bytes","descriptor_equal"),
   descriptor_differences:Object.fromEntries(["layout_different","viewport_different","capacities_different","mode_different"].map(k=>[k,sum("bytes",k)])),
   extracted_payload_bytes:sum("bytes","extracted_total"),snapshot_copy_payload_bytes:sum("bytes","copied_payload"),
   maximum_retained_payload_bytes:max("bytes","retained_payload"),peak_retained_payload_bytes:max("bytes","peak_payload"),
   rejection_counts:Object.fromEntries(["missing_or_malformed","empty","oversized","cpu_route","recovery_or_invalid_route","pending_bump_readback"].map(k=>[k,sum("rejects",k)])),
   incomplete:attempts.filter(r=>!complete(r)).map(r=>({name:r.name,exit_code:r.exit_code,issues:r.parsed_result?.issues??["missing parser result"]})),
   accepted_payload_caveat:"bounded four-entry span history; descriptor equality compares most-recent same-spans observation, not a full-key cache simulation",
  };
 });
 return {expected_runs:6,attempted_runs:runs.length,complete_diagnostic_reports:runs.filter(complete).length,
  missing:plan.filter(p=>!runs.some(r=>r.name===p.name)).map(p=>p.name),groups,
  acceptance_eligible:false,performance_improvement_measured:false,gpu_output_reuse_implemented:false,
  timings_are_diagnostic_only:true,payload_byte_totals_exclude_allocator_and_descriptor_overhead:true};
}
