# What the usability tests establish

This bench combines repeatable product checks with goal-only exploration by a vision model. It does not award a universal “good UX” score.

## Evidence layers

| Property | Evidence | What remains unproven |
| --- | --- | --- |
| Task effectiveness | Exact application state, visible document body, persisted workspace name | Whether the task is valuable to people |
| Efficiency | Clicks, keys, wheel events and decisions; declared route budgets | Human task time, cognitive effort, globally optimal route |
| Discoverability | Fresh agent receives goal and screenshots, finds its own route | Human first-use comprehension; models have prior software knowledge |
| Feedback | Before/after screenshots and independent state, repeated/no-op actions | Whether a human notices or understands the feedback |
| Recovery | Wrong query followed by successful correction; Escape cancels without changing work | Every possible error and data-loss path |
| Visual operability | Target geometry and named spacing/contrast/motion probes across configurations | Whole-screen typography, physical Retina raster quality, subjective visual quality |
| Keyboard access | Real key events complete named journeys | Full screen-reader compatibility or every control's keyboard access |
| Reliability | Fresh isolated trials and regression negative controls | Statistical generalization to people, devices or unspecified tasks |

## Experiment rules

1. Keep goal definitions and private success oracles separate from the agent prompt. Do not supply selectors, shortcut recipes, internal application state or scripted routes to the vision agent.
2. A successful model response is not a successful task. A final `done` with the wrong product state fails. Completion must have capture evidence and a satisfied independent oracle.
3. Keep task success separate from efficient success. A long or looping route can complete the task while failing its usability budget. API errors are execution errors, never product passes.
4. Preserve unsuccessful attempts alongside successful ones. Do not rerun until green and report only the last result. Name repeat counts, model, prompt version and display configuration.
5. Call these model trials, not human participants. Repeated calls to one model are not independent samples of a human population. Small-sample percentages are descriptive, not population confidence claims.
6. Distinguish screenshot/capture and application input latency from model/network deliberation. Neither is human task time or hardware input latency.
7. Negative controls must challenge the evaluator: false completion, wrong contents, absent screenshots, invalid inputs, excessive routes and loops must not earn a pass.
8. Test the real widget host continuously for each attempt. Replaying an accumulated prefix in a freshly reset app is not equivalent to interacting with one running session.

### Declared efficiency gates

| Goal | Maximum input decisions for efficient success |
| --- | --- |
| Open Document-000 from the desktop | 12 |
| Rename the workspace | 10 |
| Find and open Document-060 in Files | 18 |

All trials stop at 24 model decisions by default. Efficient success also requires no more than two unchanged captures and three repeated screenshot states. These are engineering regression thresholds declared before the trials, not measured human norms or a mathematical shortest path. A `type` decision can contain several characters; native `counts.keys` in each observation records the full character/key count separately. No-op measurements use captured-image equality and do not establish that absolutely no internal state changed.

## API and data boundary

The live runner uses the official OpenAI Responses API with image input and constrained action output. Only synthetic fixture screenshots and the task/interaction history are sent. The API key comes from an environment variable or non-echoing prompt; it is never an artifact. The action executor accepts only bounded pointer, keyboard, text and wheel inputs in the isolated shell. There is no model tool for terminal commands, filesystem reads, source code or hidden test oracles.

References: [OpenAI computer use](https://developers.openai.com/api/docs/guides/tools-computer-use), [structured outputs](https://developers.openai.com/api/docs/guides/structured-outputs).

## Interpreting a result

Read failures and coverage before the pass count. A deterministic pass supports the measured property under the named configuration. An agent failure is a reproducible friction hypothesis: inspect its screenshot/action sequence to distinguish product ambiguity, model error, an unsupported input or a faulty test. Follow up with human task sessions for comprehension and subjective ease, and live hardware checks for mouse/trackpad gesture behavior.
