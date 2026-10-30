//! The `"tl"` import table: names, wasm signatures and the dispatch tag of each import. It
//! matches the compiler's table (`tilolang::abi`); `tests/abi_parity.rs` checks that.
//!
//! Signatures are written as strings of `i` (i32, which also carries handles and bools),
//! `l` (i64) and `f` (f64): parameters, then the result (empty for none).
//!
//! Imports with a `_*` suffix come in one form per value kind (`_i`, `_f`, `_h`):
//! `tuple_set_*(t, idx: i32, x)`, `tuple_get_*(t, idx: i32)` and `rec_set_*(rec, field, x)`.

/// The module name every import lives in.
pub const MODULE: &str = "tl";

macro_rules! imports {
    ($(($variant:ident, $name:literal, $params:literal, $result:literal)),* $(,)?) => {
        /// One host import.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Imp { $($variant),* }

        /// Every import: tag, name, parameter signature, result signature.
        pub const IMPORTS: &[(Imp, &str, &str, &str)] = &[
            $((Imp::$variant, $name, $params, $result)),*
        ];
    };
}

imports! {
    (StrLit, "str_lit", "ii", "i"),
    (StrConcat, "str_concat", "ii", "i"),
    (StrEq, "str_eq", "ii", "i"),
    (StrLen, "str_len", "i", "l"),
    (IntToStr, "int_to_str", "l", "i"),
    (FloatToStr, "float_to_str", "f", "i"),
    (StrCmp, "str_cmp", "ii", "i"),

    (ListNew, "list_new", "", "i"),
    (ListPushI, "list_push_i", "il", ""),
    (ListPushF, "list_push_f", "if", ""),
    (ListPushH, "list_push_h", "ii", ""),
    (ListLen, "list_len", "i", "l"),
    (ListGetI, "list_get_i", "il", "l"),
    (ListGetF, "list_get_f", "il", "f"),
    (ListGetH, "list_get_h", "il", "i"),
    (ListSortByKey, "list_sort_by_key", "ii", "i"),
    (ListReverse, "list_reverse", "i", "i"),
    (ListEnd, "list_end", "ii", "i"),

    (TupleNew, "tuple_new", "i", "i"),
    (TupleSetI, "tuple_set_i", "iil", ""),
    (TupleSetF, "tuple_set_f", "iif", ""),
    (TupleSetH, "tuple_set_h", "iii", ""),
    (TupleGetI, "tuple_get_i", "ii", "l"),
    (TupleGetF, "tuple_get_f", "ii", "f"),
    (TupleGetH, "tuple_get_h", "ii", "i"),

    (SomeH, "some_h", "i", "i"),
    (SomeI, "some_i", "l", "i"),
    (SomeF, "some_f", "f", "i"),
    (NoneV, "none", "", "i"),
    (IsSome, "is_some", "i", "i"),
    (UnwrapI, "unwrap_i", "i", "l"),
    (UnwrapF, "unwrap_f", "i", "f"),
    (UnwrapH, "unwrap_h", "i", "i"),

    (ResGetI, "res_get_i", "ii", "l"),
    (ResGetF, "res_get_f", "ii", "f"),
    (ResGetH, "res_get_h", "ii", "i"),
    (ResSetI, "res_set_i", "iil", ""),
    (ResSetF, "res_set_f", "iif", ""),
    (ResSetH, "res_set_h", "iii", ""),
    (ResAddI, "res_add_i", "iil", ""),
    (ResAddF, "res_add_f", "iif", ""),
    (ResTextSet, "res_text_set", "iii", ""),
    (CollInsert, "coll_insert", "iii", "i"),
    (CollRemove, "coll_remove", "iii", ""),
    (CollMove, "coll_move", "iiiii", ""),
    (CollWhere, "coll_where", "ii", "i"),
    (SetAdd, "set_add", "iii", ""),
    (SetRemove, "set_remove", "iii", ""),
    (MapPut, "map_put", "iiii", ""),
    (MapRemove, "map_remove", "iii", ""),
    (MapSetAdd, "map_set_add", "iiii", ""),
    (MapSetRemove, "map_set_remove", "iiii", ""),
    (MapGetH, "map_get_h", "iii", "i"),
    (MapKeys, "map_keys", "ii", "i"),
    (RecNew, "rec_new", "i", "i"),
    (RecSetI, "rec_set_i", "iil", ""),
    (RecSetF, "rec_set_f", "iif", ""),
    (RecSetH, "rec_set_h", "iii", ""),
    (RefLit, "ref_lit", "iii", "i"),
    (RefEq, "ref_eq", "ii", "i"),
    (RefIs, "ref_is", "ii", "i"),
    (RefId, "ref_id", "i", "i"),

    (Invoker, "invoker", "", "i"),
    (PrincipalEq, "principal_eq", "ii", "i"),
    (PrincipalName, "principal_name", "i", "i"),
    (Now, "now", "", "i"),
    (PrincipalNamed, "principal_named", "i", "i"),
    (InvokerPush, "invoker_push", "ii", ""),
    (InvokerPop, "invoker_pop", "", ""),
    (PrincipalKind, "principal_kind", "i", "l"),
    (PrincipalRole, "principal_role", "i", "i"),

    (EnumNew, "enum_new", "ii", "i"),
    (EnumSetI, "enum_set_i", "iil", ""),
    (EnumSetF, "enum_set_f", "iif", ""),
    (EnumSetH, "enum_set_h", "iii", ""),
    (EnumIs, "enum_is", "ii", "i"),
    (EnumGetI, "enum_get_i", "ii", "l"),
    (EnumGetF, "enum_get_f", "ii", "f"),
    (EnumGetH, "enum_get_h", "ii", "i"),
    (ValueEq, "value_eq", "ii", "i"),

    (ActRef, "act_ref", "ii", "i"),
    (Invoke, "invoke", "i", "i"),
    (TryInvoke, "try_invoke", "i", "i"),
    (IsError, "is_error", "i", "i"),
    (Outside, "outside", "ii", "i"),
    (ModelCall, "model_call", "ii", "i"),
    (QueueBlock, "queue_block", "iii", ""),
    (Print, "print", "ii", ""),
    (Find, "find", "ii", "i"),
    (ReadText, "read_text", "i", "i"),
    (OfType, "of_type", "ii", "i"),
    (Keep, "keep", "ii", ""),
    (EditField, "edit_field", "iii", ""),
    (Grep, "grep", "iii", "i"),
    (Describe, "describe", "i", "i"),
    (DescribeRef, "describe_ref", "i", "i"),
    (ReadWith, "read_with", "iii", "i"),
    (PrintRows, "print_rows", "iii", ""),
    (CountBy, "count_by", "ii", "i"),
    (GroupBy, "group_by", "ii", "i"),
    (Take, "take", "il", "i"),
    (Single, "single", "i", "i"),
    (Me, "me", "", "i"),
    (ForeignRef, "foreign_ref", "iii", "i"),
    (MatchGroup, "match_group", "il", "i"),

    (UiNode, "ui_node", "ii", "i"),
    (UiPropH, "ui_prop_h", "iii", ""),
    (UiPropI, "ui_prop_i", "iil", ""),
    (UiPropF, "ui_prop_f", "iif", ""),
    (UiChild, "ui_child", "ii", ""),
    (UiBind, "ui_bind", "iii", ""),
    (UiOn, "ui_on", "iii", ""),
    (UiAnchor, "ui_anchor", "ii", ""),
    (UiBindState, "ui_bind_state", "iii", ""),
    (StateGet, "state_get", "iii", "i"),
    (ActSet, "act_set", "iii", "i"),
    (ActThen, "act_then", "ii", "i"),
    (ActOpen, "act_open", "iii", "i"),
    (EventValue, "event_value", "", "i"),
    (StateAt, "state_at", "iii", "i"),
    (ArgAt, "arg_at", "iii", "i"),
    (Actions, "actions", "i", "i"),
    (Lenses, "lenses", "i", "i"),
    (Delivery, "delivery", "i", "i"),
    (WorkspaceMembers, "workspace_members", "", "i"),
    (Members, "members", "i", "i"),
    (Speakers, "speakers", "i", "i"),
    (MetaOf, "meta_of", "i", "i"),
    (Versions, "versions", "i", "i"),
    (VersionEnd, "version_end", "ii", "i"),
    (RunChanges, "run_changes", "i", "i"),
    (Unseen, "unseen", "i", "i"),
    (CollOrdered, "coll_ordered", "ii", "i"),
    (CollWhereWith, "coll_where_with", "iii", "i"),
    (CollMoveTo, "coll_move_to", "iiil", ""),
    (Query, "query", "iiii", "i"),
    (Filters, "filters", "i", "i"),
    (IdentPart, "ident_part", "ii", "i"),
    (RichNew, "rich_new", "i", "i"),
    (RichPart, "rich_part", "ii", "i"),
    (RichEmpty, "rich_empty", "i", "i"),
    (RichTime, "rich_time", "i", "i"),
    (Phrase, "phrase", "ii", "i"),
    (Dated, "dated", "ii", "i"),
    (EventPart, "event_part", "i", "i"),
    (CollMoveAmong, "coll_move_among", "iiili", ""),
    (UiActions, "ui_actions", "iii", ""),
    (SpaceI, "space_i", "i", "l"),
    (SpaceF, "space_f", "i", "f"),

    (Abort, "abort", "i", ""),
}

/// The import with this table index.
pub fn imp(id: usize) -> Option<Imp> {
    IMPORTS.get(id).map(|row| row.0)
}

/// The name of an import, for messages.
pub fn name(id: usize) -> &'static str {
    IMPORTS.get(id).map_or("?", |row| row.1)
}
