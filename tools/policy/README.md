# Repository policy tooling

This directory owns fast, deterministic checks of repository structure and
cross-cutting contracts. Run the complete local and CI policy suite with:

```sh
tools/policy/run.sh
```

Build selection and execution helpers belong in `tools/ci/`; application
publication tooling belongs in `tools/apps/`.
