// Synthetic evidence mutations; not benchmark measurements.
import fs from "node:fs";
import assert from "node:assert/strict";
import {parse,audit} from "./comparison-parser.mjs";
const old=new URL("../2026-09-06-xrgb-damage-output/",import.meta.url);
const raw=name=>fs.readFileSync(new URL(name,old),"utf8");
const header=on=>"area PPT8: env_requested="+on+" renderer_requested="+on+" available="+on+" (synthetic fixture)\n";
const counts=(full,sparse)=>"area PPT8 shader-selected attempts: full="+full+" sparse="+sparse+" total="+(full+sparse)+"\n";
const sh=raw("shell-default-round-1.log"),b=raw("shell-bounds-round-1.log"),bx=raw("shell-bounds-xrgb-round-1.log"),ui=raw("ui-default-round-1.log");
const issues=(text,workload,variant)=>audit(parse(text,workload),{workload,variant});
const tests=[];function test(name,fn){fn();tests.push({name,passed:true});}
test("default shell retains no candidate selection",()=>assert.deepEqual(issues(sh+header(false)+counts(0,0),"shell","default"),[]));
test("PPT8 shell, bounds and partial-write cases retain matched controls",()=>{
 assert.deepEqual(issues(sh+header(true)+counts(1,999),"shell","ppt8"),[]);
 assert.deepEqual(issues(b+header(true)+counts(1,999),"shell","bounds-ppt8"),[]);
 assert.deepEqual(issues(bx+header(true)+counts(1,999),"shell","bounds-xrgb-ppt8"),[]);
});
test("PPT8 UI sparse selection is actual, not a no-path control",()=>assert.deepEqual(issues(ui+header(true)+counts(0,1000),"ui","ppt8"),[]));
test("requested but unused and partially selected candidate rejected",()=>{
 assert.ok(issues(sh+header(true)+counts(0,0),"shell","ppt8").includes("PPT8 actual selection mismatch"));
 assert.ok(issues(sh+header(true)+counts(1,998),"shell","ppt8").includes("eligible shell attempts were not all PPT8"));
 assert.ok(issues(ui+header(true)+counts(0,999),"ui","ppt8").includes("eligible UI sparse attempts were not all PPT8"));
});
test("missing duplicate and inconsistent counters rejected",()=>{
 assert.ok(issues(sh,"shell","default").length);
 assert.ok(issues(sh+header(false)+header(false)+counts(0,0),"shell","default").includes("missing or duplicate PPT8 evidence"));
 assert.ok(issues(sh+header(true)+counts(1,999).replace("total=1000","total=999"),"shell","ppt8").includes("PPT8 counts inconsistent"));
});
test("unplanned bounds/canonical/fine variants rejected",()=>{
 assert.ok(issues(b+header(false)+counts(0,0),"shell","default").length);
 assert.ok(issues(sh.replace("sparse solid fuse shader-selected submissions: 0","sparse solid fuse shader-selected submissions: 1")+header(true)+counts(1,999),"shell","ppt8").length);
});
test("diagnostics cannot count as ordinary comparison",()=>assert.ok(issues(sh+header(true)+counts(1,999)+"gates: mean<=25000us p99<=50000us (DisabledDiagnostic)\n","shell","ppt8").includes("instrumented diagnostic is not comparison evidence")));
test("unsafe counters and wrong adapter rejected",()=>{
 assert.ok(issues(sh+header(true)+counts(1,999).replace("full=1 ","full=9007199254740993 "),"shell","ppt8").includes("PPT8 counts inconsistent"));
 assert.ok(issues(sh.replace('backend=Vulkan','backend=Other')+header(true)+counts(1,999),"shell","ppt8").includes("adapter evidence missing or changed"));
});
console.log(JSON.stringify({synthetic_only:true,passed:tests.length,tests},null,2));
