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

Behaviour is unchanged. A crate's own generic code still compiles what it
instantiates: a `T: Deserialize` read from a `Value` is the crate's, and so is
a lookup by a key type the map is generic over (`get`, `remove`).

Upstream: none of this changes serde_json's API, but it trades inlining for
code size, which upstream's users mostly would not want. Keep the copy while
the apps link serde_json through a shared runtime.
