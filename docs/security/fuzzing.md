# Security fuzzing

Tilo continuously fuzzes the parsers that accept data before a user has decided
to trust its sender. The harnesses live in `fuzz/` and run daily under
libFuzzer's sanitizer instrumentation:

- `tapp_ingress`: signed tapp roots plus manifest syntax and semantic policy;
- `workspace_invitation`: signed workspace invitations, in bytes and as text;
- `identity_wire`: device certificates, revocations and enrollment tokens.

Run one locally with:

```sh
cargo +nightly-2026-08-25 fuzz run tapp_ingress -- -max_total_time=300
```

A crash is a release blocker. Minimized reproducers belong under
`fuzz/corpus/<target>/`; add a regression test to the owning crate before
closing the finding. Fuzzing does not replace structured protocol tests or a
cryptographic review: it is primarily evidence that hostile lengths, enum
values, malformed encodings and state combinations cannot panic or escape the
validator.
