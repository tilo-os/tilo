# Review record before validation

Two source-level reviews found no semantic producer/consumer pairing, fallback,
resource-lifetime or paint-math blocker in the production design. This was not
an assertion that the code compiled or that latency improved. The ordinary
full/CPU/shared paths retain original dispatches; the capture helper is cfg(test)
only and downloads after its matching fine dispatch, before buffer frees.

Root reviewed returned-stat-only shell/UI accounting and unchanged timing/body
rendering. Final test review required depth9 clips, actual Multiply coverage,
raw unflagged FILL checks and a64MiB PTCL download cap. The first compile attempt
then failed with seven errors and one warning, before any GPU fixture ran:

- A broad test edit put a depth condition into an older opaque-prune loop that
  has no depth variable; restore that older fixture unchanged.
- One old recording-test call was missing a new boolean argument.
- Three gradient fixtures passed an unsupported reference-to-array stop source.
- The sweep fixture used f64 TAU where the API requires f32.
- A production getter borrowed self after mutable fields entered a render call;
  precompute the requested-quad boolean before that call.
- A test log used an unnecessarily qualified size_of.

The exact command/output and first input manifest are preserved in vendor-tests.log
and validation-inputs.sha256. No GPU test or timing invocation ran under these
failed inputs. Corrective source edits are bounded to those findings and must
freeze before a separately logged second validation. No default promotion,
commit, push or goal completion is justified by a design review.
