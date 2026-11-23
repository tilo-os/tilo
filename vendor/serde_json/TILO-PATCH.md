# Tilo patch: `Value` code compiled once, in serde_json

Every thin app links the shared runtime (`libruntime_dylib.so`), which holds
serde_json. Code that is generic, or marked `#[inline]`, is still compiled
into each crate that uses it, so every app carried its own copy of the
`Value` machinery beside the runtime's. Three changes move that copy into
serde_json, where the runtime holds it once and every app calls it:

- **A raw value is read back out of line.** `ValueVisitor::visit_map` is
  generic, so it is compiled wherever a `Value` is read, from any
  deserializer (an app reading an action's arguments out of a `Value` does).
  With the `raw_value` feature, which the workspace enables, its branch for a
  `RawValue` key called `crate::from_str`, and that made every such crate
  compile a whole `Value` parser over `StrRead` too. The branch now calls
  `value_from_raw_str`, which is not generic and not inlined.
- **The map's own operations are not `#[inline]`.** `Map::insert`,
  `shift_insert`, `with_capacity`, `clear`, `append`, `Clone` and
  `PartialEq` take no type parameter, but `#[inline]` made each caller compile
  the index map's insertion, growth, cloning and comparison for
  `<String, Value>`. `FromIterator` and `Extend` stay generic over the
  iterator but loop over `insert` after reserving as the index map does (all
  of the lower size hint into an empty map, half of it otherwise), so a
  crate collecting entries compiles a loop and not the map's insertion.
- **`Value`'s `Clone` and `PartialEq` are written out.** The derived ones
  are `#[inline]`. The written ones compare and clone variant by variant
  exactly as the derives did; `Eq` and `Hash` stay derived.

- **Text is read as its bytes.** `from_str` reads through `SliceRead`, as
  `from_slice` does, instead of `StrRead`. Every type read both from text and
  from bytes compiled its whole deserializer once per reader; the browser
  desktop's start module carried about 300 KB of such twins. Reading bytes
  checks that string contents are UTF-8, which text already is, so what is
  read and every error stay the same. `Deserializer::from_str` keeps
  `StrRead`, as its type names it.

- **Reading a type out of a `Value` compiles only its visitor.** The
  sequence, map and enum readers over a `Value` (owned and borrowed) take
  their next element, key and value, check that every entry was read and
  split an enum's single-key map in functions that are not generic. Before,
  each of those was inlined into every visitor that read a struct, a list or
  an enum, and every field's value read compiled the map iterator's step.
- **Writing a type into a `Value` compiles only its `Serialize`.** A struct
  field, list element or map value is converted with `to_value` and handed to
  a function that is not generic to insert. A raw value's field is converted
  the same way and its text read back there, so no field type compiles a
  serializer that only accepts a raw value's text.

- **A struct is read out of a `Value` from an object only.** Upstream also
  reads a struct from an array of its fields in order, so every struct read
  from a `Value` compiled its visitor's sequence reader too (32 KB in Chat,
  about 150 KB over the default collection), for a form serde_json never writes
  a struct in. An array where a struct is expected is now the same
  "invalid type" error any other mismatch is. Reading text with `from_slice`
  or `from_str` still accepts both, and so do tuple structs.

Behaviour is otherwise unchanged. A crate's own generic code still compiles what it
instantiates: a `T: Deserialize` read from a `Value` is the crate's, and so is
a lookup by a key type the map is generic over (`get`, `remove`).

Upstream: none of this changes serde_json's API, but it trades inlining for
code size, which upstream's users mostly would not want. Keep the copy while
the apps link serde_json through a shared runtime.
