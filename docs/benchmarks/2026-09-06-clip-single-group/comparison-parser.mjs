// Pure parsing of ordinary comparison evidence; no runtime or acceptance action.
import {parse as parseBase,audit as auditBase} from "../2026-09-06-area-ppt8/comparison-parser.mjs";
export function parse(output,workload){
 const p=parseBase(output,workload);
 const headers=[...output.matchAll(/^clip single group: env_requested=(true|false) renderer_requested=(true|false) available=(true|false) /gm)];
 const counts=[...output.matchAll(/^clip single group shader-selected attempts: full=(\d+) sparse=(\d+) total=(\d+)$/gm)];
 const h=headers.length===1?headers[0]:null,c=counts.length===1?counts[0]:null;
 const v={header_count:headers.length,counts_count:counts.length,env_requested:h?h[1]==="true":null,renderer_requested:h?h[2]==="true":null,available:h?h[3]==="true":null,full:c?+c[1]:null,sparse:c?+c[2]:null,total:c?+c[3]:null};
 v.counts_consistent=!!c&&[v.full,v.sparse,v.total].every(n=>Number.isSafeInteger(n)&&n>=0)&&v.full+v.sparse===v.total;
 const measured=[...output.matchAll(/^sparse submission accounting \(measured workload\):.*?\bclip_single_group_shader_selected=(\d+)\b/gm)];
 return {...p,clip_single_group:v,clip_single_measured_sparse:measured.length===1?+measured[0][1]:null};
}
export function audit(p,spec){
 const issues=auditBase(p,spec),want=spec.variant.includes("clip-single"),v=p.clip_single_group;
 if(v.header_count!==1||v.counts_count!==1)issues.push("missing or duplicate clip-single telemetry");
 if(v.env_requested!==want||v.renderer_requested!==want||v.available!==want)issues.push("clip-single request/availability mismatch");
 if(!v.counts_consistent)issues.push("clip-single counts inconsistent");
 if(want?!(v.total>0):v.total!==0)issues.push("clip-single actual selection mismatch");
 if(spec.workload==="shell"&&(!p.attempts||v.full>p.attempts.full||v.sparse>p.attempts.sparse||v.total>p.attempts.total))issues.push("clip-single count exceeds recorded shell attempts");
 if(spec.workload==="ui"&&(p.clip_single_measured_sparse!==v.sparse||!Number.isSafeInteger(p.clip_single_measured_sparse)||v.sparse>p.sparse?.total))issues.push("clip-single UI sparse accounting mismatch");
 return issues;
}
