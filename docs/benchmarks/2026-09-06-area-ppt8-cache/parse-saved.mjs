import {readFile} from "node:fs/promises";
import {parse,audit} from "../2026-09-06-area-ppt8/comparison-parser.mjs";
const run=JSON.parse(await readFile(process.argv[2],"utf8"));
const raw=await readFile(run.log,"utf8");
const p=parse(raw,"shell");
console.log(JSON.stringify({...run,parsed:p,comparison_content_issues:audit(p,run),errors:raw.split("\n").filter(x=>x.startsWith("Error:")),cache_stat_lines:raw.split("\n").filter(x=>!/Command being timed:|^\$ /.test(x)&&/cache.*(hit|miss)|hit.*miss/i.test(x))},null,2));
