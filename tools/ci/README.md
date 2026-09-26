# CI tooling

This directory owns the implementation used only to plan, execute, measure,
and report continuous-integration work. It includes affected-package
selection, compiler-cache setup, lint ratchets, builder resolution, and CI
receipts.

`check_base.py` picks a main check run's selection base: the last main commit
whose own check run started, so a run covers commits whose queued runs
`queue_coalesce.py` cancelled.

Repository policy and release utilities remain in `tools/`; their behavior is
useful outside CI. Tests for both groups remain in `tools/tests/` so the single
repository-policy discovery command exercises the complete tooling surface.

Workflows should invoke these files through `tools/ci/...`. When adding a new
CI helper, place it here and add focused coverage under `tools/tests/` rather
than growing the flat `tools/` namespace again.
