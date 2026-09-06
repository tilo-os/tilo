import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { parse } from './parse.mjs';

export const median = values => {
  const sorted = values.toSorted((a,b)=>a-b);
  const middle = Math.floor(sorted.length/2);
  return sorted.length ? sorted.length%2 ? sorted[middle] : (sorted[middle-1]+sorted[middle])/2 : null;
};
const allFinite = (rows, field) => rows.every(row => Number.isFinite(row[field]));
export function summarize(rows) {
  const groups = [];
  for (const key of new Set(rows.map(row=>[row.workload,row.revision,row.configuration].join('/')))) {
    const all=rows.filter(row=>[row.workload,row.revision,row.configuration].join('/')===key);
    const good=all.filter(row=>row.complete);
    const stats=field=>median(good.map(row=>row[field]).filter(Number.isFinite));
    const worst_us=good.length && allFinite(good,'max_us') ? Math.max(...good.map(row=>row.max_us)) : null;
    groups.push({key,attempts:all.length,complete:good.length,mean_us:stats('mean_us'),p99_us:stats('p99_us'),peak_rss_kib:stats('peak_rss_kib'),process_wall_s:stats('process_wall_s'),worst_us,numeric_gate_passes:good.filter(row=>row.numeric_gate_pass===true).length,failed_attempts:all.filter(row=>!row.complete).map(row=>row.name)});
  }
  const comparisons=[];
  for (const candidate of groups.filter(row=>row.key.includes('/branch/'))) {
    const workload=candidate.key.split('/')[0];
    const baseline=groups.find(row=>row.key===workload+'/main/default');
    const comparable=baseline?.complete===3 && candidate.complete===3;
    const delta=field=>comparable && baseline[field]>0 && Number.isFinite(candidate[field]) ? (candidate[field]/baseline[field]-1)*100 : null;
    const frozen=workload==='ui-frozen';
    comparisons.push({baseline:baseline?.key,candidate:candidate.key,all_three_completed:comparable,mean_time_change_percent:frozen?null:delta('mean_us'),p99_change_percent:delta('p99_us'),peak_process_rss_change_percent:delta('peak_rss_kib'),process_wall_time_change_percent:delta('process_wall_s'),caveat:workload==='shell'?'main does not immediately verify/retry each frame overflow; timing is not pixel-equivalence proof':frozen?'mean-time percent suppressed: branch mean is integer-microsecond truncated while main is fractional; process wall remains comparable, with setup/reporting and differently sized benchmark storage included':'benchmark-only no-readback adapter; independent main changes prevent single-patch causal attribution'});
  }
  return {groups,comparisons,attempts:rows};
}
if (process.argv[1] && import.meta.url === new URL('file://'+process.argv[1]).href) {
  const plan=JSON.parse(readFileSync(process.argv[2],'utf8'));
  const rows=plan.jobs.filter(job=>job.kind==='benchmark').map(job=>{
    const meta=JSON.parse(readFileSync(job.log+'.result.json','utf8'));
    return parse(job,meta,readFileSync(job.log));
  });
  assert.equal(rows.length,21,'all predeclared attempts must have a result');
  const summary=summarize(rows);
  const output=join(dirname(process.argv[2]),'comparison-summary.json');
  writeFileSync(output,JSON.stringify(summary,null,2)+'\n',{flag:'wx'});
  console.log(JSON.stringify(summary.groups,null,2));
  console.log('Saved '+output);
}
