# Generated metadata parser correction

The initial candidate audit failed before runtime because it expected
`binding_indices:` but Cargo emits `binding_indices :` (a space before the
colon). The generated field is present and has the same indices as P4. The
field parser now accepts whitespace before the colon; no shader was changed.
The initial script and raw error are preserved. The initial audit JSON records
the failure and links its raw error log; use the corrected JSON artifact for
the successful audit.

The corrected check verifies seven old permutations' token/metadata hashes,
both new workgroup sizes [2,16,1], and identical P4/P8 bindings, workgroup-buffer
metadata and binding indices. This is generated-source/metadata evidence only.
