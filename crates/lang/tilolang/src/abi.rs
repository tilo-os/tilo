//! The host imports, module `"tl"`: every function a compiled module may call, with its wasm
//! signature.
//!
//! `Int` is an `i64`, `Float` an `f64`, `Bool` an `i32`. Every other value (text, lists, records,
//! references, actions, UI nodes) lives in the host and the guest holds an `i32` handle to it, so a
//! module can't forge a reference or an action: it can only name handles the host gave it. Fields,
//! types and actions are named by their ids, passed as string literal handles. An import traps on
//! misuse (a wrong kind of handle, a mutation outside an action); well-typed code never makes such
//! a call, apart from a reference literal that doesn't resolve. The runtime's table
//! (`tilolang_runtime::abi`) must match this one.

use crate::types::Vt;

macro_rules! imports {
    ($($v:ident = $n:literal ([$($p:ident),*] -> [$($r:ident),*]);)*) => {
        /// One import of the `"tl"` module.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Imp { $($v),* }

        impl Imp {
            /// Every import, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$v),*];

            /// The import's name in the module.
            pub const fn name(self) -> &'static str {
                match self { $(Self::$v => $n),* }
            }

            pub const fn params(self) -> &'static [Vt] {
                match self { $(Self::$v => &[$(Vt::$p),*]),* }
            }

            pub const fn results(self) -> &'static [Vt] {
                match self { $(Self::$v => &[$(Vt::$r),*]),* }
            }
        }
    };
}

imports! {
    StrLit = "str_lit" ([I32, I32] -> [H]);
    StrConcat = "str_concat" ([H, H] -> [H]);
    StrEq = "str_eq" ([H, H] -> [I32]);
    StrLen = "str_len" ([H] -> [I64]);
    IntToStr = "int_to_str" ([I64] -> [H]);
    FloatToStr = "float_to_str" ([F64] -> [H]);
    StrCmp = "str_cmp" ([H, H] -> [I32]);

    ListNew = "list_new" ([] -> [H]);
    ListPushI = "list_push_i" ([H, I64] -> []);
    ListPushF = "list_push_f" ([H, F64] -> []);
    ListPushH = "list_push_h" ([H, H] -> []);
    ListLen = "list_len" ([H] -> [I64]);
    ListGetI = "list_get_i" ([H, I64] -> [I64]);
    ListGetF = "list_get_f" ([H, I64] -> [F64]);
    ListGetH = "list_get_h" ([H, I64] -> [H]);
    ListSortByKey = "list_sort_by_key" ([H, H] -> [H]);
    ListReverse = "list_reverse" ([H] -> [H]);
    ListEnd = "list_end" ([H, I32] -> [H]);

    TupleNew = "tuple_new" ([I32] -> [H]);
    TupleSetI = "tuple_set_i" ([H, I32, I64] -> []);
    TupleSetF = "tuple_set_f" ([H, I32, F64] -> []);
    TupleSetH = "tuple_set_h" ([H, I32, H] -> []);
    TupleGetI = "tuple_get_i" ([H, I32] -> [I64]);
    TupleGetF = "tuple_get_f" ([H, I32] -> [F64]);
    TupleGetH = "tuple_get_h" ([H, I32] -> [H]);

    SomeH = "some_h" ([H] -> [H]);
    SomeI = "some_i" ([I64] -> [H]);
    SomeF = "some_f" ([F64] -> [H]);
    NoneV = "none" ([] -> [H]);
    IsSome = "is_some" ([H] -> [I32]);
    UnwrapI = "unwrap_i" ([H] -> [I64]);
    UnwrapF = "unwrap_f" ([H] -> [F64]);
    UnwrapH = "unwrap_h" ([H] -> [H]);

    ResGetI = "res_get_i" ([H, H] -> [I64]);
    ResGetF = "res_get_f" ([H, H] -> [F64]);
    ResGetH = "res_get_h" ([H, H] -> [H]);
    ResSetI = "res_set_i" ([H, H, I64] -> []);
    ResSetF = "res_set_f" ([H, H, F64] -> []);
    ResSetH = "res_set_h" ([H, H, H] -> []);
    ResAddI = "res_add_i" ([H, H, I64] -> []);
    ResAddF = "res_add_f" ([H, H, F64] -> []);
    ResTextSet = "res_text_set" ([H, H, H] -> []);

    CollInsert = "coll_insert" ([H, H, H] -> [H]);
    CollRemove = "coll_remove" ([H, H, H] -> []);
    CollMove = "coll_move" ([H, H, H, H, I32] -> []);
    CollWhere = "coll_where" ([H, H] -> [H]);
    SetAdd = "set_add" ([H, H, H] -> []);
    SetRemove = "set_remove" ([H, H, H] -> []);
    MapPut = "map_put" ([H, H, H, H] -> []);
    MapRemove = "map_remove" ([H, H, H] -> []);
    MapSetAdd = "map_set_add" ([H, H, H, H] -> []);
    MapSetRemove = "map_set_remove" ([H, H, H, H] -> []);
    MapGetH = "map_get_h" ([H, H, H] -> [H]);
    MapKeys = "map_keys" ([H, H] -> [H]);
    RecNew = "rec_new" ([H] -> [H]);
    RecSetI = "rec_set_i" ([H, H, I64] -> []);
    RecSetF = "rec_set_f" ([H, H, F64] -> []);
    RecSetH = "rec_set_h" ([H, H, H] -> []);
    RefLit = "ref_lit" ([H, H, H] -> [H]);
    RefEq = "ref_eq" ([H, H] -> [I32]);
    RefIs = "ref_is" ([H, H] -> [I32]);
    RefId = "ref_id" ([H] -> [H]);

    Invoker = "invoker" ([] -> [H]);
    PrincipalEq = "principal_eq" ([H, H] -> [I32]);
    PrincipalName = "principal_name" ([H] -> [H]);
    Now = "now" ([] -> [H]);
    PrincipalNamed = "principal_named" ([H] -> [H]);
    InvokerPush = "invoker_push" ([H, H] -> []);
    InvokerPop = "invoker_pop" ([] -> []);
    PrincipalKind = "principal_kind" ([H] -> [I64]);
    PrincipalRole = "principal_role" ([H] -> [H]);

    EnumNew = "enum_new" ([H, H] -> [H]);
    EnumSetI = "enum_set_i" ([H, H, I64] -> []);
    EnumSetF = "enum_set_f" ([H, H, F64] -> []);
    EnumSetH = "enum_set_h" ([H, H, H] -> []);
    EnumIs = "enum_is" ([H, H] -> [I32]);
    EnumGetI = "enum_get_i" ([H, H] -> [I64]);
    EnumGetF = "enum_get_f" ([H, H] -> [F64]);
    EnumGetH = "enum_get_h" ([H, H] -> [H]);
    ValueEq = "value_eq" ([H, H] -> [I32]);

    ActRef = "act_ref" ([H, H] -> [H]);
    Invoke = "invoke" ([H] -> [H]);
    TryInvoke = "try_invoke" ([H] -> [H]);
    Outside = "outside" ([H, H] -> [H]);
    ModelCall = "model_call" ([H, H] -> [H]);
    QueueBlock = "queue_block" ([H, H, H] -> []);
    Print = "print" ([H, H] -> []);
    Find = "find" ([H, H] -> [H]);
    ReadText = "read_text" ([H] -> [H]);
    OfType = "of_type" ([H, H] -> [H]);
    Keep = "keep" ([H, H] -> []);
    EditField = "edit_field" ([H, H, H] -> []);
    Grep = "grep" ([H, H, H] -> [H]);
    Describe = "describe" ([H] -> [H]);
    DescribeRef = "describe_ref" ([H] -> [H]);
    ReadWith = "read_with" ([H, H, H] -> [H]);
    PrintRows = "print_rows" ([H, H, H] -> []);
    CountBy = "count_by" ([H, H] -> [H]);
    GroupBy = "group_by" ([H, H] -> [H]);
    Take = "take" ([H, I64] -> [H]);
    Single = "single" ([H] -> [H]);
    Me = "me" ([] -> [H]);
    ForeignRef = "foreign_ref" ([H, H, H] -> [H]);
    MatchGroup = "match_group" ([H, I64] -> [H]);

    UiNode = "ui_node" ([H, H] -> [H]);
    UiPropH = "ui_prop_h" ([H, H, H] -> []);
    UiPropI = "ui_prop_i" ([H, H, I64] -> []);
    UiPropF = "ui_prop_f" ([H, H, F64] -> []);
    UiChild = "ui_child" ([H, H] -> []);
    UiBind = "ui_bind" ([H, H, H] -> []);
    UiOn = "ui_on" ([H, H, H] -> []);
    UiAnchor = "ui_anchor" ([H, H] -> []);
    UiBindState = "ui_bind_state" ([H, I32, H] -> []);
    StateGet = "state_get" ([I32, H, H] -> [H]);
    ActSet = "act_set" ([I32, H, H] -> [H]);
    ActThen = "act_then" ([H, H] -> [H]);
    ActOpen = "act_open" ([H, H, I32] -> [H]);
    ActUndo = "act_undo" ([H, I32] -> [H]);
    ChangeView = "change_view" ([H, I32, H] -> [H]);
    EventValue = "event_value" ([] -> [H]);
    StateAt = "state_at" ([I32, H, I32] -> [H]);
    ArgAt = "arg_at" ([H, H, I32] -> [H]);
    Actions = "actions" ([H] -> [H]);
    Lenses = "lenses" ([H] -> [H]);
    Delivery = "delivery" ([H] -> [H]);
    WorkspaceMembers = "workspace_members" ([] -> [H]);
    Members = "members" ([H] -> [H]);
    Speakers = "speakers" ([H] -> [H]);
    MetaOf = "meta_of" ([H] -> [H]);
    Versions = "versions" ([H] -> [H]);
    VersionEnd = "version_end" ([H, I32] -> [H]);
    RunChanges = "run_changes" ([H] -> [H]);
    Unseen = "unseen" ([H] -> [H]);
    Steps = "steps" ([H] -> [H]);
    CollOrdered = "coll_ordered" ([H, H] -> [H]);
    CollWhereWith = "coll_where_with" ([H, H, H] -> [H]);
    CollMoveTo = "coll_move_to" ([H, H, H, I64] -> []);
    Query = "query" ([H, H, H, H] -> [H]);
    Filters = "filters" ([H] -> [H]);
    IdentPart = "ident_part" ([H, I32] -> [H]);
    RichNew = "rich_new" ([H] -> [H]);
    RichPart = "rich_part" ([H, I32] -> [H]);
    RichEmpty = "rich_empty" ([H] -> [I32]);
    RichTime = "rich_time" ([H] -> [H]);
    Phrase = "phrase" ([H, H] -> [H]);
    Dated = "dated" ([H, H] -> [H]);
    EventPart = "event_part" ([I32] -> [H]);
    CollMoveAmong = "coll_move_among" ([H, H, H, I64, H] -> []);
    UiActions = "ui_actions" ([H, H, H] -> []);

    SpaceI = "space_i" ([H] -> [I64]);
    SpaceF = "space_f" ([H] -> [F64]);

    Abort = "abort" ([H] -> []);
}
