// Reparse all planned raw logs and check their saved metadata before aggregation.
// Does not run a benchmark or write files. Shell gate failures need their raw
// Error line, so output is deliberately rehydrated rather than dropped.
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
import {parse, audit} from "./comparison-parser.mjs";
import {summarize} from "./comparison-summary.mjs";

const read = name => fs.readFileSync(new URL(name, import.meta.url), "utf8");
const plan = JSON.parse(read("comparison-plan-v2.json"));
assert.equal(plan.length, 30);
const runs = plan.map(spec => {
  const basename = path.basename(spec.log);
  assert.ok(basename.endsWith(".log"));
  const saved = JSON.parse(read(basename.slice(0, -4) + ".result.json"));
  for (const [key, value] of Object.entries(spec)) {
    assert.deepEqual(saved[key], value, `${spec.name}: metadata/plan mismatch in ${key}`);
  }
  const output = read(basename);
  assert.ok(output.startsWith("$ " + spec.cmd + "\n\n"), `${spec.name}: command mismatch`);
  assert.ok(output.endsWith(`Recorded process exit: ${saved.exit_code}\n`), `${spec.name}: exit wrapper mismatch`);
  const parsed = parse(output, spec.workload);
  const evidence_issues = audit(parsed, spec);
  assert.deepEqual(parsed, saved.parsed, `${spec.name}: saved parsing differs from raw log`);
  assert.deepEqual(evidence_issues, saved.evidence_issues, `${spec.name}: saved evidence issues differ`);
  return {...saved, output, parsed, evidence_issues};
});
console.log(JSON.stringify(summarize(runs), null, 2));
