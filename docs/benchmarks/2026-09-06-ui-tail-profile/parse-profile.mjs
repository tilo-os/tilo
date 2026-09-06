import {readFile} from "node:fs/promises";
import {parse as baseParse} from "../2026-09-06-area-ppt8/comparison-parser.mjs";
export function parse(output) {
 const base=baseParse(output,"ui");
 const phases=Object.fromEntries([...output.matchAll(/^(handle_event|layout \(gated\)|draw_to_scene|vello render|TOTAL\/event)\s+(\d+)\s+(\d+)\s+(\d+)$/gm)].map(m=>[m[1],{mean_us:+m[2],p95_us:+m[3],p99_us:+m[4]}]));
 const metrics=Object.fromEntries([...output.matchAll(/^  ([\w*-]+)\s+mean=(\d+)(us|ns|count|px|tiles|groups) p99=(\d+)\3$/gm)].filter(m=>!["duration","dispatches"].includes(m[1])).map(m=>[m[1],{mean:+m[2],p99:+m[4],unit:m[3]}]));
 const stages=[...output.matchAll(/^  dispatch-stage-(\w+) \(id=(\d+)\): complete_samples=(\d+) total_dispatches=(\d+)\n  duration\s+mean=(\d+)ns p99=(\d+)ns\n  dispatches\s+mean=(\d+)count p99=(\d+)count$/gm)].map(m=>({name:m[1],id:+m[2],complete_samples:+m[3],dispatches:+m[4],mean_ns:+m[5],p99_ns:+m[6]}));
 const gpu=output.match(/^  gpu samples: complete=(\d+)\/(\d+) omitted=(\d+); submissions expected=(\d+) received=(\d+) missing=(\d+);/m);
 const dispatch=output.match(/^  gpu dispatch timestamps: status=(\w+); complete=(\d+)\/(\d+) omitted=(\d+); submissions expected=(\d+) received=(\d+) missing=(\d+) stale=(\d+);/m);
 const slowest=[...output.matchAll(/^  (\d+): (\d+)\/(\d+)\/(\d+)\/(\d+)\/(\d+)\/(\d+)\/(\d+); sparse=(\d+)\/(\d+)\/(\d+)\/(\d+); gpu=span=(\d+)ns last-compute=(\d+)ns other-compute=(\d+)ns complete \((\d+)\/(\d+)\/(\d+) received\/expected\/missing\); (\d+), (\d+)$/gm)].map(m=>Object.fromEntries(["index","render_us","submit_us","copy_us","setup_us","creates","settle_us","unaccounted_us","sparse_submits","fine_tiles","written_pixels","coarse_prefix_pixels","gpu_span_ns","last_compute_ns","other_compute_ns","gpu_received","gpu_expected","gpu_missing","full_attempts","raster_pixels"].map((k,i)=>[k,+m[i+1]])));
 const totalTail=[...output.matchAll(/^  (\d+): (\d+), (\d+)$/gm)].map(m=>({index:+m[1],total_us:+m[2],raster_pixels:+m[3]}));
 return {base,phases,metrics,stages,gpu:gpu?gpu.slice(1).map(Number):null,dispatch:dispatch?{status:dispatch[1],counts:dispatch.slice(2).map(Number)}:null,slowest,totalTail};
}
export function audit(p,spec,output){
 const issues=[];
 if(!output.includes("=== Dashboard bench (1000 events, canvas 3840×2160) ===")||!/^redrawn events:\s+1000 /m.test(output))issues.push("missing native 4K/1000 redraws");
 if(Object.keys(p.phases).length!==5)issues.push("missing phase data");
 if(!p.gpu||JSON.stringify(p.gpu)!==JSON.stringify([1000,1000,0,1000,1000,0]))issues.push("GPU timestamp coverage incomplete");
 if(!p.dispatch||JSON.stringify(p.dispatch.counts)!==JSON.stringify([1000,1000,0,1000,1000,0,0]))issues.push("dispatch coverage incomplete");
 if(p.stages.length!==10||p.stages.some(x=>x.complete_samples!==1000||x.dispatches!==1000))issues.push("stage coverage changed");
 if(p.slowest.length!==10||new Set(p.slowest.map(x=>x.index)).size!==10||p.totalTail.length!==10)issues.push("tail rows incomplete");
 if(p.slowest.some(x=>x.gpu_expected!==1||x.gpu_received!==1||x.gpu_missing!==0||x.full_attempts!==0||x.sparse_submits!==1))issues.push("tail sample matching/recovery changed");
 if(!/^full\/recovery\s+0 samples$/m.test(output)||!/^partial\s+1000 /m.test(output))issues.push("frame population changed");
 const want=spec.variant==="ppt8",a=p.base.ppt8;
 if(!a?.counts_consistent||a.available!==want||a.renderer_requested!==want||a.env_requested!==want||a.full!==0||a.sparse!==(want?1000:0))issues.push("PPT8 selection mismatch");
 if(p.base.cpu_readback_bytes!==0)issues.push("CPU readback changed");
 if(/^frame-budget:|^redraw-mean-budget:/m.test(output))issues.push("unexpected acceptance gate");
 return issues;
}
if(process.argv[2]){
 const spec=JSON.parse(await readFile(process.argv[2],"utf8")),output=await readFile(spec.log,"utf8");
 const parsed=parse(output);console.log(JSON.stringify({name:spec.name,variant:spec.variant,round:spec.round,exit_code:spec.exit_code,acceptance_eligible:false,parsed,issues:audit(parsed,spec,output)},null,2));
}
