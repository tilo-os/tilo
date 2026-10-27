# Verifiers: what a search row promises, and what a run claims

A search row is a promise — *this is the thing you named* — and a run's
receipt is a claim — *the task is done*. Both are graded, and both are graded
from state the system holds, never from what the provider or the run says
about itself. This note is what the graders are, which adversarial states
each one refuses, and what is measured rather than asserted.

## The two graders in the tree

| Grader | What it reads | Where |
| --- | --- | --- |
| Row verifier | The retained record corpus (which rooms hold or minted an id) | `crates/session/shell/src/shell/search/verifier.rs` |
| Action-argument verifier | The action's declared parameters: names, types, required, choices, JSON Schema | `ActionArgs::bind_json` in `crates/collab/actions/src/params.rs` |

The row verifier is also what the Records provider's Enter handler asks
before it brings a record into a workspace, so a row a person or an agent
clicks and the same row an agent invokes by address are judged the same way.

A third grader is the Writer agent benchmark
(`apps/word-processor/tests/writer_benchmark.rs`): it grades two arms of one
task from the document the system owns — kinds, levels, list kinds, text,
formatted phrases, pixels — and never from what an arm reports.

## Positive and negative cases

Every grader has both, in the same file as the grader.

| Grader | Positive | Negative |
| --- | --- | --- |
| Row verifier | `a_row_whose_record_a_room_holds_passes` | `a_row_whose_record_no_room_holds_is_refused`, `a_row_that_names_no_record_address_is_refused`, `a_row_of_an_unobserved_kind_is_refused_not_passed` |
| Argument verifier | `a_call_with_the_declared_arguments_verifies` | `a_call_outside_the_declared_parameters_is_refused` |
| Writer benchmark | `writer_agent_benchmark_qualifies_against_the_reference` | `a_document_that_misses_the_reference_is_not_qualified`, `a_page_missing_one_format_is_not_the_same_picture` |

The adversarial states named below are each one of those negative cases.

## The adversarial states

- **Showing the expected text without saving it.** The row verifier asks the
  corpus on every Enter, so a row that still names a record after the last
  room dropped it is refused, though the page that produced it is unchanged.
  The Writer benchmark `snapshot()` reads the document the system owns, so a
  receipt, a status line or a frame is never evidence that an edit landed;
  `visual.editor_draws_the_document` is a separate qualification precisely
  because drawing is not saving.
- **Answers padded with every candidate.** The argument verifier refuses an
  argument the action never declared (`ArgError::Unknown`), a value outside
  the declared choices, a missing required parameter and a wrong type, so
  `[everything that could be right]` is not a passing answer.
- **Declaring infeasible on a feasible task.** No grader in the tree has to
  believe a run's own account of what is possible: a claim is admitted only
  when the state it names is observable, so "it cannot be done" is never
  graded as a pass and never graded as success either. A run that stops has
  produced no state, and no positive qualification is recorded for it.
- **No evidence is not evidence of agreement.** A row whose kind the
  verifier holds no state for comes back `Unverifiable`, which is not a pass.

## Agreement with people

The row verifier's human-facing outcome is the Enter handler: a person
clicking a stale row sees a refusal beside an agent getting one. The
benchmark's qualifications are the sample the tree grades on its own, and
`docs/benchmarks/writer-agent-benchmark-2026-10-14.md` records a full run
with its receipt. Measuring agreement with human labels on a sample of agent
trajectories needs trajectories recorded outside this repository, and so is
tracked where the trajectories live rather than here.
