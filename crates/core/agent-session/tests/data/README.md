# Instruction mode fixtures

Test-only data; nothing here is embedded in the engine.

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

The harness is `tests/instruction_mode_eval.rs`: it reads this set, runs every
instruction through the production `respond` transform with a scripted model,
and holds the decision to its budget (`RESPOND_TIMEOUT` in
`src/transform.rs`, 6 s, the repair attempt included). It also checks that the
set carries both decisions in numbers and repeats no instruction, so a fixed
answer cannot score well.

A recording against a real provider is a measurement, not a test: a model
answers the set and its decisions are compared with the expectations. Against
the default local model the image ships; a recording against a hosted provider
does not qualify the local one.
