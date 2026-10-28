# Instruction mode fixtures

Test-only data; nothing here is embedded in the agent.

| File | Contents |
|------|----------|
| `instruction_mode_eval.jsonl` | The evaluation set for a request's decision: one instruction per line with the context it is given, the app it came from, and whether it is answered in place (`one-shot`) or carried out as work (`task`). |

## What the decision is

The composer asks the agent's `respond` action about an instruction before it
runs anything. The agent answers `{"answer": …}` when the instruction is a
single derived value for the context it was given (rewrite, translate,
summarise, explain) and `{"work": …}` when carrying it out needs actions,
lookups or resources beyond that context. The person can overrule either way
from the composer's mode picker, which is why a misclassification is cheap but
a slow one is not: the composer is a box the person is waiting in front of.

## Scoring the set

The scorer is `apps/agent/tests/instruction_mode_eval.rs`. It reads this set,
compares a recorded decision per instruction with the expected one, and reports
accuracy and the per-mode split. A recording is produced by the agent's own
`respond` transform against a real provider:

```sh
cargo run --example instruction_mode_eval -p agent -- --record /tmp/decisions.jsonl
cargo run --example instruction_mode_eval -p agent -- --score /tmp/decisions.jsonl
```

`--score` prints `accuracy`, `one-shot` recall and `task` recall, and the
median and p95 latency of the decision, which is the budget the decision has to
meet (`RESPOND_TIMEOUT` in `apps/agent/src/transform.rs`, 6 s, the repair
attempt included).

Record against the default local model the image ships; a recording against a
hosted provider says so in its own `model` field and does not qualify the local
one.
