// Synthetic aggregation checks only; never benchmark evidence.
import assert from "node:assert/strict";
import {summarize,complete} from "./comparison-summary.mjs";
const row=(workload="shell",variant="default",round=1)=>({name:[workload,variant,round].join("-"),workload,variant,round,exit_code:workload==="shell"?1:2,output_truncated:false,evidence_issues:[],
 output:'Error: "redraw gate failed (synthetic)"',parsed:{exact_gate:"FAIL",exact_mean_us:30000.5,stats:{count:1000,mean_us:30000,p99_us:60000,max_us:100000,gate:"FAIL"},peak_rss_kib:500000,known_gpu_bytes:100,xrgb:{bytes:33177600000}}});
const tests=[];
function test(name,fn){fn();tests.push({name,passed:true});}
test("complete failed gates stay in medians",()=>{const s=summarize([row()]);assert.equal(s.complete,1);assert.equal(s.groups[0].median_mean_ms,30.0005);assert.equal(s.groups[0].numeric_passes,0);});
test("timeouts remain attempted, not completed",()=>{const r=row();r.exit_code=124;const s=summarize([r]);assert.equal(s.attempted,1);assert.equal(s.complete,0);assert.equal(s.groups[0].attempts,1);assert.equal(s.groups[0].median_mean_ms,null);});
test("truncated and mismatched evidence rejected",()=>{const a=row();a.output_truncated=true;const b=row("shell","ppt8");b.evidence_issues=["not actually selected"];assert.equal(summarize([a,b]).complete,0);});
test("exact shell gate, not rounded mean, controls pass",()=>{const r=row();r.parsed.exact_mean_us=25000.001;r.parsed.stats.mean_us=25000;r.parsed.stats.p99_us=30000;assert.equal(summarize([r]).groups[0].numeric_passes,0);});
test("UI complete failure retained and success recognized",()=>{const a=row("ui"),b=row("ui","ppt8");b.exit_code=0;b.parsed.stats.gate="PASS";b.parsed.stats.p99_us=29000;const s=summarize([a,b]);assert.equal(s.complete,2);assert.equal(s.groups.find(g=>g.workload==="ui"&&g.variant==="ppt8").numeric_passes,1);});
test("unexpected exits and missing reports fail completion",()=>{const a=row();a.exit_code=0;assert.equal(complete(a),false);const b=row();b.parsed.stats.count=999;assert.equal(complete(b),false);const c=row();c.output='Error: "runtime error"';assert.equal(complete(c),false);});
test("all repeated medians and paired means use exact values",()=>{const runs=[];for(let i=1;i<=3;i++){const b=row("shell","bounds",i),c=row("shell","bounds-ppt8",i);b.parsed.exact_mean_us=i*10000;c.parsed.exact_mean_us=i*8000;runs.push(b,c);}const s=summarize(runs),p=s.pairs.find(p=>p.base==="bounds"&&p.workload==="shell");assert.equal(s.complete,6);assert.ok(Math.abs(p.ratio_of_medians.mean_change_percent+20)<1e-9);assert.equal(p.paired.length,3);});
test("duplicate or unplanned attempts rejected",()=>{assert.throws(()=>summarize([row(),row()]),/duplicate/);assert.throws(()=>summarize([row("shell","default",4)]),/Unexpected/);});
console.log(JSON.stringify({synthetic_only:true,passed:tests.length,tests},null,2));
