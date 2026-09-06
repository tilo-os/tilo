import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";
import {parse,audit} from "./parse-profile.mjs";
const raw=await readFile(new URL("./ui-default-round-1.log",import.meta.url),"utf8");
const spec={variant:"default"};
assert.equal(audit(parse(raw),spec,raw).length,0);
const changes=[
 raw.replace("complete=1000/1000 omitted=0","complete=999/1000 omitted=1"),
 raw.replace("missing=0 stale=0","missing=1 stale=0"),
 raw.replace("redrawn events:  1000","redrawn events:  999"),
 raw.replace("canvas 3840×2160","canvas 1920×1080"),
 raw.replace("total_dispatches=1000","total_dispatches=999"),
 raw.replace("CPU readback bytes: 0","CPU readback bytes: 16"),
 raw.replace("area PPT8 shader-selected attempts: full=0 sparse=0 total=0","area PPT8 shader-selected attempts: full=0 sparse=1 total=1"),
 raw.replace("  17: 130951/604","  18: 130951/604").replace("  221: 118291/696","  18: 118291/696"),
 raw+"\nframe-budget: PASS\n"
];
for(const [i,changed] of changes.entries())assert.ok(audit(parse(changed),spec,changed).length>0,"mutation "+i+" must be rejected");
console.log(JSON.stringify({fixtures:10,passed:10,kind:"baseline plus nine missing/mismatched evidence mutations"}));
