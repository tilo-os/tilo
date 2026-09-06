module.exports = (runs) => {
 const median=xs=>{const a=xs.filter(Number.isFinite).sort((a,b)=>a-b);return a.length?(a[Math.floor((a.length-1)/2)]+a[Math.ceil((a.length-1)/2)])/2:null;};
 const expectedExit=r=>r.workload==="shell"
  ? (r.parsed.exact_gate==="PASS"?r.exit_code===0:r.parsed.exact_gate==="FAIL"&&r.exit_code===1&&/^Error: "redraw gate failed /m.test(r.output??""))
  : (r.parsed.stats?.gate==="PASS"?r.exit_code===0:r.parsed.stats?.gate==="FAIL"&&r.exit_code===2);
 const complete=r=>expectedExit(r) && !r.output_truncated && r.evidence_issues.length===0 && r.parsed.stats?.count===1000;
 const mean=r=>r.workload==="shell"?r.parsed.exact_mean_us:r.parsed.stats?.mean_us;
 const good=r=>complete(r) && (r.workload==="shell"?r.parsed.exact_gate==="PASS":r.parsed.stats.p99_us<=30000);
 const variants=[["shell","default"],["shell","xrgb"],["shell","bounds"],["shell","bounds-xrgb"],["shell","bounds-canonical"],["shell","bounds-canonical-xrgb"],["ui","default"],["ui","xrgb"]];
 const groups=variants.map(([workload,variant])=>{
  const all=runs.filter(r=>r.workload===workload&&r.variant===variant), ok=all.filter(complete);
  return {workload,variant,attempts:all.length,planned:3,complete:ok.length,numeric_passes:all.filter(good).length,
   median_mean_ms:median(ok.map(r=>mean(r)/1000)),median_p99_ms:median(ok.map(r=>r.parsed.stats.p99_us/1000)),
   worst_sample_ms:ok.length?Math.max(...ok.map(r=>r.parsed.stats.max_us/1000)):null,
   median_peak_rss_kib:median(ok.map(r=>r.parsed.peak_rss_kib)),
   median_cpu_xrgb_bytes:workload==="shell"?median(ok.map(r=>r.parsed.xrgb.bytes)):null,
   median_cpu_xrgb_write_reduction_percent:workload==="shell"?median(ok.map(r=>r.parsed.xrgb.cpu_write_reduction_percent)):null,
   known_gpu_bytes:[...new Set(ok.map(r=>r.parsed.known_gpu_bytes).filter(Number.isFinite))],
   issues:all.filter(r=>!complete(r)).map(r=>({name:r.name,exit_code:r.exit_code,issues:r.evidence_issues,full_summary:r.parsed.stats?.count===1000})),
   acceptance_eligible:false};
 });
 const comparisonPairs=[["shell","default","xrgb"],["shell","bounds","bounds-xrgb"],["shell","bounds-canonical","bounds-canonical-xrgb"],["ui","default","xrgb"]];
 const pairs=comparisonPairs.map(([workload,base,candidate])=>{
  const paired=[1,2,3].map(round=>{
   const b=runs.find(r=>r.workload===workload&&r.variant===base&&r.round===round);
   const c=runs.find(r=>r.workload===workload&&r.variant===candidate&&r.round===round);
   if(!b||!c||!complete(b)||!complete(c))return {round,complete:false};
   return {round,complete:true,mean_change_percent:100*(mean(c)/mean(b)-1),p99_change_percent:100*(c.parsed.stats.p99_us/b.parsed.stats.p99_us-1),rss_change_percent:100*(c.parsed.peak_rss_kib/b.parsed.peak_rss_kib-1),cpu_xrgb_byte_change_percent:workload==="shell"?100*(c.parsed.xrgb.bytes/b.parsed.xrgb.bytes-1):null};
  });
  const b=groups.find(g=>g.workload===workload&&g.variant===base),c=groups.find(g=>g.workload===workload&&g.variant===candidate);
  return {workload,base,candidate,paired,ratio_of_medians:{mean_change_percent:b.median_mean_ms&&c.median_mean_ms?100*(c.median_mean_ms/b.median_mean_ms-1):null,p99_change_percent:b.median_p99_ms&&c.median_p99_ms?100*(c.median_p99_ms/b.median_p99_ms-1):null,rss_change_percent:b.median_peak_rss_kib&&c.median_peak_rss_kib?100*(c.median_peak_rss_kib/b.median_peak_rss_kib-1):null},causal_ui_gain_claim_allowed:false};
 });
 return {attempted:runs.length,planned:24,complete:runs.filter(complete).length,groups,pairs,acceptance_eligible:false,notes:["Repeated matched runs; all attempts retained. Negative change means less time/RSS/CPU output, not guaranteed causation.","XRGB affects shell CPU destination writes, not GPU readback, physical bus transfer or peak VRAM. UI X is a no-path regression control.","Known GPU bytes are reported accounting endpoints, not hardware peak memory. Peak RSS is whole-process resident memory.","Exact shell mean uses timing total/sample count; UI mean uses ordinary reported integer microseconds.","Current machine is aarch64 with Vulkan llvmpipe; no physical 2010 laptop, KMS presentation, or constant-60FPS proof."]};
};
