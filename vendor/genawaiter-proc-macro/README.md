# Tilo genawaiter diagnostic compatibility fork

This is the `genawaiter-proc-macro` 0.99.1 implementation used by
`genawaiter` 0.99.1. It removes the unmaintained `proc-macro-error` dependency
and emits the same compile failures with `syn::Error::to_compile_error`, and it
parses with syn 2, the version the rest of the workspace builds, instead of
syn 1.

The public macro API and successful expansion output are unchanged. Remove
this patch when `iroh-blobs` and `bao-tree` no longer depend on `genawaiter`.
