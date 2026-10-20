//! A package as the compiler's metadata describes it, and its registered schemas.

use tilolang_tilo::PackageMeta;

/// The counter of the language spec, plus a todo list for collections, sets and text.
pub const META: &str = r#"{
  "app": "so.tilo.counter", "title": "Counter", "module_hash": "mod1",
  "types": [
    {"id": "tcnt0001", "name": "Counter", "fields": [
      {"id": "fval0001", "name": "value", "type": "Int", "merge": "counter"}]},
    {"id": "ttodo001", "name": "Todo", "fields": [
      {"id": "ftitle01", "name": "title", "type": "Text", "merge": "text"},
      {"id": "fdone001", "name": "done", "type": "Bool", "merge": "register", "default": false},
      {"id": "fitems01", "name": "items", "type": "Collection<Item>"},
      {"id": "ftags001", "name": "tags", "type": "Set<Text>"},
      {"id": "fprops01", "name": "props", "type": "Map<Text>"}]},
    {"id": "titem001", "name": "Item", "fields": [
      {"id": "ftext001", "name": "text", "type": "Text", "merge": "text"},
      {"id": "fcheck01", "name": "checked", "type": "Bool", "merge": "register", "default": false}]}
  ],
  "actions": [
    {"id": "aincr001", "name": "increment", "params": [{"name": "c", "type": "Counter"}],
     "class": "undoable", "doc": "Count one up."},
    {"id": "aadd00001", "name": "add", "params": [{"name": "t", "type": "Todo"}, {"name": "text", "type": "Text"}],
     "class": "undoable"},
    {"id": "alook001", "name": "peek", "params": [{"name": "c", "type": "Counter"}],
     "class": "read"},
    {"id": "asend001", "name": "announce", "params": [{"name": "message", "type": "Text"}],
     "class": "outside"}
  ],
  "views": [{"id": "vmain001", "name": "main", "params": [{"name": "c", "type": "Counter"}]}]
}"#;

pub fn meta() -> PackageMeta {
    let meta = PackageMeta::from_json(META).unwrap();
    meta.register_schemas();
    meta
}
