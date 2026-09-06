// Pure evidence parsing for the predeclared PPT8 comparison.
import {parse as parseBase,audit as auditBase} from "../2026-09-06-xrgb-damage-output/comparison-parser.js";
export function parse(output,workload) {
 const p=parseBase(output,workload);
 const headers=[...output.matchAll(/^area PPT8: env_requested=(true|false) renderer_requested=(true|false) available=(true|false) /gm)];
 const counts=[...output.matchAll(/^area PPT8 shader-selected attempts: full=(\d+) sparse=(\d+) total=(\d+)$/gm)];
 const h=headers.length===1?headers[0]:null,c=counts.length===1?counts[0]:null;
 const field=(match,index)=>match?Number(match[index]):null;
 const ppt8={header_count:headers.length,counts_count:counts.length,
  env_requested:h?h[1]==="true":null,renderer_requested:h?h[2]==="true":null,available:h?h[3]==="true":null,
  full:field(c,1),sparse:field(c,2),total:field(c,3)};
 ppt8.counts_consistent=!!c&&[ppt8.full,ppt8.sparse,ppt8.total].every(Number.isSafeInteger)&&ppt8.full+ppt8.sparse===ppt8.total;
 return {...p,ppt8,diagnostic:/^diagnostic: QOS_REDRAW_BENCH_PROFILE=1|^diagnostic phase means:|^gates:.*DisabledDiagnostic/m.test(output)};
}
export function audit(p,spec) {
 const issues=auditBase(p,spec);
 const want=spec.variant.includes("ppt8"),v=p.ppt8;
 if(p.diagnostic)issues.push("instrumented diagnostic is not comparison evidence");
 if(v.header_count!==1||v.counts_count!==1)issues.push("missing or duplicate PPT8 evidence");
 if(v.env_requested!==want||v.renderer_requested!==want||v.available!==want)issues.push("PPT8 request/availability mismatch");
 if(!v.counts_consistent)issues.push("PPT8 counts inconsistent");
 if(want?!(v.total>0):v.total!==0)issues.push("PPT8 actual selection mismatch");
 if(spec.workload==="shell"&&want) {
  if(v.full!==p.attempts?.full||v.sparse!==p.attempts?.sparse||v.total!==p.attempts?.total)
   issues.push("eligible shell attempts were not all PPT8");
 }
 if(spec.workload==="ui"&&want && v.sparse!==p.sparse?.total)issues.push("eligible UI sparse attempts were not all PPT8");
 if(spec.variant.includes("bounds")&&!(p.sparse?.applied>0))issues.push("no actual bounds selection");
 for(const [name,value] of Object.entries(p.stats??{}))if(typeof value==="number"&&!Number.isSafeInteger(value))issues.push("unsafe timing value: "+name);
 if(p.stats?.count!==1000||!(p.stats?.mean_us>0)||!(p.stats?.p99_us>0)||!(p.stats?.max_us>=p.stats?.p99_us))issues.push("invalid timing statistics");
 return issues;
}
