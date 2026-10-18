# Tilo patch: Unicode tables the loader does not relocate

ucd-generate writes regex-syntax's Unicode tables as slices of references:

- each property table's `BY_NAME: &[(&str, &[(char, char)])]` (about 520
  names over seven tables),
- `PROPERTY_NAMES: &[(&str, &str)]` (271 aliases),
- `PROPERTY_VALUES: &[(&str, &[(&str, &str)])]` (908 values),
- `CASE_FOLDING_SIMPLE: &[(char, &[char])]` (2938 scalars).

Every reference is a pointer the dynamic loader writes at load time, so the
tables are about 121 KB of `.data.rel.ro` that every process mapping the
library copies and relocates before `main`. In Tilo that library is the
shared app runtime (`libruntime_dylib.so`, through `regex`): every app paid
for them whether or not it ever compiled `\p{Greek}` or `(?i)`.

`tools/regex-syntax-tables.py` rewrites them without pointers. A name table
is one string literal of the names in order, a table of `(u16, u8)` spans
into it, and a `by_name` function that binary-searches the spans and returns
the set through a `match` on the index (code, not data, so nothing to
relocate). The property names and values become string literals with
`(u16, u8, u16, u8)` offset tables, behind `canonical`, `property_values`
and `canonical_value`. The case folding table becomes `(char, u16, u8)` rows
into one `[char]` of the equivalence classes, and `SimpleCaseFolder` slices
it. The range sets themselves (`&[(char, char)]` constants) were already
plain data and are unchanged, as is every name, alias, value and fold.

Upstream: the same layout would fit ucd-generate's `--chars` output; offer
it there and drop this copy once a release carries it.
