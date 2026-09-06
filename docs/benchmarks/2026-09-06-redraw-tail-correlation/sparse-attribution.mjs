// Post-capture diagnostic stratification, not a replacement for 1000-action totals.
import fs from "node:fs";
import {fileURLToPath} from "node:url";
import {parseRecords,analyze} from "./analyze-capture.mjs";
export function sparseAttribution(recordPath) {
  const rows=parseRecords(fs.readFileSync(recordPath,"utf8"));
  const sparse=rows.filter(r=>r.sparse_present && r.full_attempts===0 && r.recovery_attempts===0);
  const full=rows.filter(r=>r.full_attempts>0);
  const a=analyze(sparse);
  const threshold=sparse.filter(r=>r.wall_us>50000);
  const categories=["gpu_fine_ns","gpu_coarse_ns","gpu_other_stage_ns","gpu_unknown_stage_ns"];
  const largestTimedCategory={};
  for(const r of threshold) {
    const key=r.timestamp_exact_match?categories.toSorted((a,b)=>r[b]-r[a])[0]:"unmatched";
    largestTimedCategory[key]=(largestTimedCategory[key]??0)+1;
  }
  return {diagnostic_only:true,post_capture_analysis:true,total_actions:rows.length,
    sparse_actions:sparse.length,full_action_indices:full.map(r=>r.index),
    other_actions:rows.length-sparse.length-full.length,
    full_rows:full,analysis:a,over_50ms:{count:threshold.length,largest_timed_category:largestTimedCategory,
      note:"Largest of timed GPU categories, not proof it caused the wall delay; categories do not cover all work/waits."},
    caveat:"All 1000-action totals, first full frame, outliers and raw data remain preserved in the primary analysis. This subset is for attribution only, never acceptance."};
}
if(process.argv[1]===fileURLToPath(import.meta.url)) console.log(JSON.stringify(sparseAttribution(process.argv[2]),null,2));
