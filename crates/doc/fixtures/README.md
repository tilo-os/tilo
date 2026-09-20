# Shared document fixtures

These files are the canonical binary fixtures for document crates and their
cross-domain consumers. Keep one copy here rather than embedding identical
office packages in each consuming crate.

- `sample.docx` and `sample.odt` are genuine office packages with the same
  heading, styled runs, list, table, and metadata structure.
- `xxe.docx` and `xxe.odt` exercise external-entity rejection.
- `zipbomb.docx` exercises decompression and package-size limits.

Tests should reference these files with `include_bytes!`, so they remain
hermetic and do not depend on the process working directory.
