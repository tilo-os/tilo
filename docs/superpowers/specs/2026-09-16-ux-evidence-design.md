# Evidence for desktop usability

## Objective
Test as much of good UX as deterministic measurement permits, and use real API agents to attempt tasks in the application without a supplied route. Passing is evidence for named properties, never a universal UX certificate.

## Design
Keep the existing deterministic matrix and add a persistent JSON-lines session to its real native widget host. A Python evaluator drives that host using only mouse, keyboard and wheel inputs. The evaluator holds outcome oracles privately. A vision agent sees the task and screenshots, not selectors, internal state, solutions or oracle results. A separate deterministic suite uses the same host for recovery, feedback, keyboard access and geometry checks. Reports distinguish product failures, agent failures, execution errors and unmeasured properties.

Alternative approaches rejected: replaying a supplied route cannot measure discovery; model-only screenshot scoring cannot verify success. Use actual state plus recorded pixels and actions instead.

## Binding requirements
- One persistent real shell session per attempt; isolated fixture HOME/profile, synthetic documents only.
- JSONL commands: observe, click(x,y), key(key,modifiers), type(text), wheel(x,y,dx,dy), finish. Commands operate visible input only; no arbitrary code, filesystem commands or semantic shortcuts.
- Responses: {ok,sequence,screenshot,inspection,state,counts,error}; paths absolute, state same existing native snapshot. Observation includes screenshot and inspection paths, without exposing these internals to the model. Finish persists and reports persisted_workspace_names.
- Fail closed for malformed/nonfinite/out-of-bounds inputs; bound command count/text length. Preserve artifacts and protocol stdout purity.
- Deterministic checks cover existing contrast/spacing/motion plus task success, keyboard route, escape recovery, wrong search recovery, visible feedback and state preservation. Missing measurements fail or are explicitly uncovered.
- Agents get a plain-language goal and pixels, no solution. Fresh attempts for at least open-file, rename-workspace and scroll-to-file goals. Repeat attempts, baseline and compact/large-text coverage. Independently verify exact file text/provider or persisted workspace name. Never accept agent self-report as success.
- Track input costs, no-op and repeated-state loops, outcome, capture/input latency separately from API latency, model and prompt identity, token use, per-attempt screenshots/actions and reproducible replay.
- Fixed step/API-call/token limits; no secrets or authorization headers in logs/artifacts/source. API key read from environment or non-echoing prompt, passed only to official OpenAI API. No real user data sent.
- Negative controls must show false completion, blank/invalid observations, excessive routes, missing feedback and failed outcomes cannot pass. Live API attempts required; mocks alone do not satisfy the objective.

## Acceptance
Run host protocol tests, evaluator negative controls, deterministic usability scenarios against the built app and live API attempts. Retain failures honestly; a working test need not declare the current application good. Publish an HTML evidence report with property coverage, denominators, limitations and concrete observed friction.
