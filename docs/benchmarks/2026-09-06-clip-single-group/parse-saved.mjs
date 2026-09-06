// Parse a saved attempt; this performs no rendering or acceptance mutation.
import fs from "node:fs";
import {parse,audit} from "./comparison-parser.mjs";
const plan=JSON.parse(fs.readFileSync(new URL("comparison-plan.json",import.meta.url),"utf8"));
const spec=plan.find(x=>x.name===process.argv[2]);
if(!spec)throw Error("Unknown predeclared comparison attempt");
const output=fs.readFileSync(spec.log,"utf8"),parsed=parse(output,spec.workload);
const evidence_issues=audit(parsed,spec);
const numeric_gate_pass=spec.workload==="shell"?parsed.exact_gate==="PASS":!!parsed.stats&&parsed.stats.p99_us<=30000;
console.log(JSON.stringify({parsed,evidence_issues,numeric_gate_pass},null,2));
