import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import {parse,audit} from "./comparison-parser.mjs";
const old=new URL("../2026-09-06-area-ppt8/",import.meta.url);
const source={shell:await readFile(new URL("shell-default-round-2.log",old),"utf8"),ui:await readFile(new URL("ui-default-round-2.log",old),"utf8")};
function fixture(workload,on){
 let text=source[workload],sparse=on?(workload==="shell"?999:1000):0,full=on&&workload==="shell"?1:0;
 if(workload==="ui")text=text.replace(/^(sparse submission accounting \(measured workload\):.*)$/m,"$1 clip_single_group_shader_selected="+sparse);
 text+="\nclip single group: env_requested="+on+" renderer_requested="+on+" available="+on+" (synthetic fixture)\nclip single group shader-selected attempts: full="+full+" sparse="+sparse+" total="+(full+sparse)+"\n";
 return{text,spec:{workload,variant:on?"clip-single":"default",expected_actions:1000,native_extent:[3840,2160]}};
}
let passed=0;
for(const workload of ["shell","ui"])for(const on of [false,true]){
 const f=fixture(workload,on);assert.deepEqual(audit(parse(f.text,workload),f.spec),[]);passed++;
}
const f=fixture("shell",true);
const bad=[
 f.text.replace("clip single group: env_requested=true","clip single group: env_requested=false"),
 f.text.replace("clip single group shader-selected attempts: full=1 sparse=999 total=1000","clip single group shader-selected attempts: full=1 sparse=999 total=1001"),
 f.text.replace("clip single group shader-selected attempts: full=1 sparse=999 total=1000","clip single group shader-selected attempts: full=0 sparse=0 total=0"),
 f.text.replace("clip single group shader-selected attempts: full=1 sparse=999 total=1000","clip single group shader-selected attempts: full=2 sparse=999 total=1001"),
 f.text+"\nclip single group shader-selected attempts: full=1 sparse=999 total=1000\n",
 f.text.replace("clip single group shader-selected attempts: full=1 sparse=999 total=1000","clip single group shader-selected attempts: full=1 sparse=9007199254740993 total=9007199254740994"),
 f.text+"\ndiagnostic: QOS_REDRAW_BENCH_PROFILE=1\n",
 f.text.replace(/clip single group:.*\n/,"")
];
for(const t of bad){assert.ok(audit(parse(t,"shell"),f.spec).length>0);passed++;}
const u=fixture("ui",true),badUi=u.text.replace("clip_single_group_shader_selected=1000","clip_single_group_shader_selected=999");
assert.ok(audit(parse(badUi,"ui"),u.spec).length>0);passed++;
console.log(JSON.stringify({synthetic_fixtures:passed,passed,actual_benchmark_runs:0}));
