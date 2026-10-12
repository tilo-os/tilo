Historical byte fixtures are stored as LF hexadecimal text (`.hex`). Decode
space-separated byte pairs before loading them. These retain the original
encoded bytes, including malformed lengths and compressed chunks; generating
new documents with the current writer would lose the regression coverage.
The sibling `fuzz-crashers` directory contains only encoded corpus entries.
