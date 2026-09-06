// Pure aggregation of predeclared attempts; includes complete gate failures.
const variants=[
 ["shell","default"],["shell","ppt8"],["shell","bounds"],["shell","bounds-ppt8"],["shell","bounds-xrgb"],["shell","bounds-xrgb-ppt8"],
 ["ui","default"],["ui","ppt8"],["ui","bounds"],["ui","bounds-ppt8"]
];
const comparisonPairs=[["shell","default","ppt8"],["shell","bounds","bounds-ppt8"],["shell","bounds-xrgb","bounds-xrgb-ppt8"],["ui","default","ppt8"],["ui","bounds","bounds-ppt8"]];
const median=values=>{const v=values.filter(Number.isFinite).toSorted((a,b)=>a-b);return v.length?(v[Math.floor((v.length-1)/2)]+v[Math.ceil((v.length-1)/2)])/2:null;};
const mean=r=>r.workload==="shell"?r.parsed.exact_mean_us:r.parsed.stats?.mean_us;
const expectedExit=r=>r.workload==="shell"
 ? (r.parsed?.exact_gate==="PASS"?r.exit_code===0:r.parsed?.exact_gate==="FAIL"&&r.exit_code===1&&/^Error: "redraw gate failed /m.test(r.output??""))
 : (r.parsed?.stats?.gate==="PASS"?r.exit_code===0:r.parsed?.stats?.gate==="FAIL"&&r.exit_code===2);
export const complete=r=>expectedExit(r)&&r.output_truncated===false&&r.evidence_issues?.length===0&&r.parsed?.stats?.count===1000;
const numericPass=r=>complete(r)&&(r.workload==="shell"?r.parsed.exact_gate==="PASS":r.parsed.stats.p99_us<=30000);
const change=(base,candidate)=>Number.isFinite(base)&&base>0&&Number.isFinite(candidate)?100*(candidate/base-1):null;
export function summarize(runs) {
 const seen=new Set();
 for(const r of runs){
  const key=JSON.stringify([r.workload,r.variant,r.round]);
  if(seen.has(key)||![1,2,3].includes(r.round)||!variants.some(([w,v])=>w===r.workload&&v===r.variant))throw Error("Unexpected or duplicate attempt: "+key);
  seen.add(key);
 }
 const groups=variants.map(([workload,variant])=>{
  const all=runs.filter(r=>r.workload===workload&&r.variant===variant), ok=all.filter(complete);
  return {workload,variant,attempts:all.length,planned:3,complete:ok.length,numeric_passes:all.filter(numericPass).length,
   median_mean_ms:median(ok.map(r=>mean(r)/1000)),median_p99_ms:median(ok.map(r=>r.parsed.stats.p99_us/1000)),
   worst_sample_ms:ok.length?Math.max(...ok.map(r=>r.parsed.stats.max_us/1000)):null,
   median_peak_rss_kib:median(ok.map(r=>r.parsed.peak_rss_kib)),
   median_cpu_xrgb_bytes:workload==="shell"?median(ok.map(r=>r.parsed.xrgb.bytes)):null,
   known_gpu_bytes:[...new Set(ok.map(r=>r.parsed.known_gpu_bytes).filter(Number.isFinite))],
   issues:all.filter(r=>!complete(r)).map(r=>({name:r.name,exit_code:r.exit_code,issues:r.evidence_issues??[],full_summary:r.parsed?.stats?.count===1000})),acceptance_eligible:false};
 });
 const pairs=comparisonPairs.map(([workload,base,candidate])=>{
  const paired=[1,2,3].map(round=>{
   const b=runs.find(r=>r.workload===workload&&r.variant===base&&r.round===round);
   const c=runs.find(r=>r.workload===workload&&r.variant===candidate&&r.round===round);
   if(!b||!c||!complete(b)||!complete(c))return {round,complete:false};
   return {round,complete:true,mean_change_percent:change(mean(b),mean(c)),p99_change_percent:change(b.parsed.stats.p99_us,c.parsed.stats.p99_us),
    rss_change_percent:change(b.parsed.peak_rss_kib,c.parsed.peak_rss_kib),cpu_xrgb_byte_change_percent:workload==="shell"?change(b.parsed.xrgb.bytes,c.parsed.xrgb.bytes):null};
  });
  const b=groups.find(g=>g.workload===workload&&g.variant===base),c=groups.find(g=>g.workload===workload&&g.variant===candidate);
  return {workload,base,candidate,paired,ratio_of_medians:{mean_change_percent:change(b.median_mean_ms,c.median_mean_ms),p99_change_percent:change(b.median_p99_ms,c.median_p99_ms),rss_change_percent:change(b.median_peak_rss_kib,c.median_peak_rss_kib)}};
 });
 return {attempted:runs.length,planned:30,complete:runs.filter(complete).length,groups,pairs,acceptance_eligible:false,
  notes:["All attempted runs and all samples preserved, including complete numeric gate failures. Crashes/timeouts remain in attempt denominators, not treated as complete reports.",
   "Matched PPT8 comparisons within identical renderer/XRGB configurations. Negative changes mean less observed time or RSS, not a promised hardware-independent speedup.",
   "Known GPU bytes are accounting endpoints, not measured peak VRAM; PPT8 private-register spilling is not captured by these counters. RSS is whole-process resident memory.",
   "Shell means use exact timing total / sample count; UI means use ordinary reported integer microseconds.",
   "aarch64 Vulkan llvmpipe host: no physical 2010-laptop, hardware-GPU, KMS presentation or constant-60FPS proof."]};
}
