# ai-bench

Quality cases for Tilo's AI interactions, and the run that asks them of one or
more models. The cases are the point: a run is only as meaningful as what it
can refuse.

## A case

A case is an instruction, an input, and what a right answer must satisfy — in
terms a program checks, never by a person (or a model) reading it:

| check | what it means |
| --- | --- |
| `shape` | the answer is a value of this JSON Schema, in the same vocabulary an action's arguments use |
| `equal` | the answer is this value |
| `text` | the answer is this text, up to surrounding whitespace |
| `containing` / `not_containing` | the answer says, or never says, this — the check an injected instruction is built to trip |
| `at_most_chars` | the answer is at most this long |
| `not_echoing_input` | no this-many words of the input appear in the answer in order: an answer that repeats its input has not transformed it |
| `reasoned` | the answer names the decision *and* gives a reason from the expected vocabulary |

Every check runs on every answer and every refusal is reported, so a report
says what the model did rather than a fraction. A model judge may add a
**named** check — one more row in the same report — and never replace a typed
one. Nothing here scores by reading.

A case also records where it came from when it was captured from a real run: a
verdict (good or bad) and, for a bad run, the wrong answer. That is how a
suite grows from what happens instead of from what a prompt author imagined;
`why` says what each case earns its place by.

## Running

```bash
# every suite, against a local model: no key, no cost, in CI
tilo-ai-bench --suite crates/dev/ai-bench/cases/summarise.json \
    --endpoints crates/dev/ai-bench/endpoints.example.json \
    --endpoint local

# one hosted model, cost printed beside the score, report kept as JSON
tilo-ai-bench --suite crates/dev/ai-bench/cases/classify.json \
    --endpoints endpoints.json --endpoint hosted-small --key-env HOSTED_KEY \
    --out report.json
```

The exit status is non-zero when a case failed, so a job gates on it. The JSON
report names the endpoint, the model it said it served, the digest of each
request, every refusal with its reason, the tokens spent and the cost — enough
for a program to compare two models, two skill versions or two Shortcut
versions, and for a person to see which endpoint answered rather than which
one was configured.

**No silent fallbacks.** An endpoint that cannot be reached, refuses the
request or answers in the wrong shape fails its cases; the run says why and
asks nothing else. Keys come from the variable `--key-env` names and never from
the environment at large, so a run cannot pick up whichever credential
happened to be around.

## Before changing a default

Run the suites against the model, skill or Shortcut version you mean to switch
to, and against the one in use, on the same commit. Compare the reports; keep
both. A run whose endpoints file changed is not a comparison, which is why the
report records the URL, the served model and each request's digest.
