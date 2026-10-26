# Repository policy tooling

This directory owns fast, deterministic checks of repository structure and
cross-cutting contracts. Run the complete local and CI policy suite with:

```sh
tools/policy/run.sh
```

Build selection and execution helpers belong in `tools/ci/`; application
publication tooling belongs in `tools/apps/`.

A check prints its findings on stdout, one per line, `path:line: message` or
`path: message`, and everything else (headings, counts, hints, OK lines) on
stderr: `findings.py` counts every stdout line as a finding. Its docstring has
the details.
