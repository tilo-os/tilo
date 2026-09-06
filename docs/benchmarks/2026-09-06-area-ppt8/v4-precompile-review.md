# v4 precompile review

No validation command was run against v4. Static review found that the extracted
helper unnecessarily makes two Option<&mut usize> parameters mutable and calls
as_deref_mut even though each option is consumed only once. The original code
matched each option directly. Terra is asked to preserve that simpler pattern
before the next frozen validation version; no behavior or timing change.
