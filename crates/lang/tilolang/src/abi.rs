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
    StrToInt = "str_to_int" ([H] -> [H]);
    FloatToStr = "float_to_str" ([F64] -> [H]);
    DurationText = "duration_text" ([I64] -> [H]);
    StrCmp = "str_cmp" ([H, H] -> [I32]);
    StrFind = "str_find" ([H, H] -> [H]);
    StrSlice = "str_slice" ([H, I64, I64] -> [H]);
    StrReplace = "str_replace" ([H, H, H] -> [H]);
    StrTrim = "str_trim" ([H] -> [H]);
    StrLower = "str_lower" ([H] -> [H]);
    StrUpper = "str_upper" ([H] -> [H]);
    StrStartsWith = "str_starts_with" ([H, H] -> [I32]);
    StrEndsWith = "str_ends_with" ([H, H] -> [I32]);
    StrContains = "str_contains" ([H, H] -> [I32]);
    StrSplit = "str_split" ([H, H] -> [H]);
    StrJoin = "str_join" ([H, H] -> [H]);
    StrMatches = "str_matches" ([H, H] -> [I32]);

    IntMin = "int_min" ([I64, I64] -> [I64]);
    IntMax = "int_max" ([I64, I64] -> [I64]);
    IntClamp = "int_clamp" ([I64, I64, I64] -> [I64]);
    IntAbs = "int_abs" ([I64] -> [I64]);
    FloatMin = "float_min" ([F64, F64] -> [F64]);
    FloatMax = "float_max" ([F64, F64] -> [F64]);
    FloatClamp = "float_clamp" ([F64, F64, F64] -> [F64]);
    FloatAbs = "float_abs" ([F64] -> [F64]);
    FloatRound = "float_round" ([F64] -> [I64]);
    FloatRoundTo = "float_round_to" ([F64, I64] -> [F64]);
    FloatFloor = "float_floor" ([F64] -> [I64]);
    FloatFloorTo = "float_floor_to" ([F64, I64] -> [F64]);
    FloatCeil = "float_ceil" ([F64] -> [I64]);
    FloatCeilTo = "float_ceil_to" ([F64, I64] -> [F64]);
    ListSumI = "list_sum_i" ([H] -> [I64]);
    ListSumF = "list_sum_f" ([H] -> [F64]);
    ListAvg = "list_avg" ([H] -> [F64]);

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
    ListIndexOf = "list_index_of" ([H, H] -> [H]);
    ListPrevious = "list_previous" ([H, H] -> [H]);

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
    // An argument a call left out before one it gives: no value, completed at the invocation.
    MissingV = "missing" ([] -> [H]);
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
    CollRelatives = "coll_relatives" ([H, H, H, H, I32] -> [H]);
    CollCount = "coll_count" ([H] -> [I64]);
    CollCounted = "coll_counted" ([H] -> [H]);
    CollEmpty = "coll_empty" ([H] -> [I32]);
    SetAdd = "set_add" ([H, H, H] -> []);
    SetRemove = "set_remove" ([H, H, H] -> []);
    MapPut = "map_put" ([H, H, H, H] -> []);
    MapRemove = "map_remove" ([H, H, H] -> []);
    MapSetAdd = "map_set_add" ([H, H, H, H] -> []);
    MapSetRemove = "map_set_remove" ([H, H, H, H] -> []);
    MapGetH = "map_get_h" ([H, H, H] -> [H]);
    MapKeys = "map_keys" ([H, H] -> [H]);
    MapGetValue = "map_get_value" ([H, H] -> [H]);
    MapKeysValue = "map_keys_value" ([H] -> [H]);
    RecNew = "rec_new" ([H] -> [H]);
    RecSetI = "rec_set_i" ([H, H, I64] -> []);
    RecSetF = "rec_set_f" ([H, H, F64] -> []);
    RecSetH = "rec_set_h" ([H, H, H] -> []);
    RefLit = "ref_lit" ([H, H, H] -> [H]);
    RefEq = "ref_eq" ([H, H] -> [I32]);
    RefIs = "ref_is" ([H, H] -> [I32]);
    RefId = "ref_id" ([H] -> [H]);
    ResourceRef = "resource_ref" ([H] -> [H]);

    Invoker = "invoker" ([] -> [H]);
    PrincipalEq = "principal_eq" ([H, H] -> [I32]);
    PrincipalName = "principal_name" ([H] -> [H]);
    Now = "now" ([] -> [H]);
    WebGet = "web_get" ([H, H, H] -> [H]);
    WebPost = "web_post" ([H, H, H] -> [H]);
    WebOk = "web_ok" ([H] -> [I32]);
    WebStatus = "web_status" ([H] -> [I64]);
    WebStatusText = "web_status_text" ([H] -> [H]);
    WebHeaders = "web_headers" ([H] -> [H]);
    WebBody = "web_body" ([H] -> [H]);
    WebText = "web_text" ([H] -> [H]);
    BytesLength = "bytes_length" ([H] -> [I64]);
    JsonParse = "json_parse" ([H] -> [H]);
    JsonDecode = "json_decode" ([H, H, H] -> [H]);
    JsonEncode = "json_encode" ([H] -> [H]);
    TimeShift = "time_shift" ([H, I64, I32] -> [H]);
    WallClock = "wall_clock" ([H] -> [H]);
    PrincipalNamed = "principal_named" ([H] -> [H]);
    AgentNamed = "agent_named" ([H] -> [H]);
    InvocationPush = "invocation_push" ([H, H, H] -> []);
    InvocationPop = "invocation_pop" ([] -> []);
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
    Suggest = "suggest" ([H] -> []);
    ProposalNote = "proposal_note" ([H] -> [H]);
    CallFn = "call_fn" ([H, H] -> [H]);
    Outside = "outside" ([H, H] -> [H]);
    OutsideAt = "outside_at" ([H, H, H] -> [H]);
    Withdraw = "withdraw" ([H, H] -> [H]);
    ModelCall = "model_call" ([H, H] -> [H]);
    Notify = "notify" ([H, H, H, H] -> []);
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
    UiPropPic = "ui_prop_picture" ([H, H, H] -> []);
    UiChild = "ui_child" ([H, H] -> []);
    UiBind = "ui_bind" ([H, H, H] -> []);
    UiOn = "ui_on" ([H, H, H] -> []);
    UiAnchor = "ui_anchor" ([H, H] -> []);
    UiBindState = "ui_bind_state" ([H, H] -> []);
    StateGet = "state_get" ([H, H] -> [H]);
    ActSet = "act_set" ([H, H] -> [H]);
    ActThen = "act_then" ([H, H] -> [H]);
    ActMadeFrom = "act_made_from" ([H, H] -> [H]);
    ActMany = "act_many" ([H] -> [H]);
    ActOpen = "act_open" ([H, H, I32] -> [H]);
    ActUndo = "act_undo" ([H, I32] -> [H]);
    ChangeView = "change_view" ([H, I32, H] -> [H]);
    EventValue = "event_value" ([] -> [H]);
    StateAt = "state_at" ([H, I32] -> [H]);
    ArgAt = "arg_at" ([H, H, I32] -> [H]);
    Actions = "actions" ([H, H] -> [H]);
    Lenses = "lenses" ([H] -> [H]);
    Delivery = "delivery" ([H] -> [H]);
    WorkspaceMembers = "workspace_members" ([] -> [H]);
    Viewer = "viewer" ([] -> [H]);
    Members = "members" ([H] -> [H]);
    Speakers = "speakers" ([H] -> [H]);
    MetaOf = "meta_of" ([H] -> [H]);
    Versions = "versions" ([H] -> [H]);
    Links = "links" ([H] -> [H]);
    VersionEnd = "version_end" ([H, I32] -> [H]);
    FieldChanged = "field_changed" ([H, H] -> [H]);
    RunChanges = "run_changes" ([H] -> [H]);
    Unseen = "unseen" ([H] -> [H]);
    FirstUnseen = "first_unseen" ([H, H, H] -> [H]);
    SeenBy = "seen_by" ([H, H] -> [H]);
    Mentions = "mentions" ([H, H, H] -> [I64]);
    Steps = "steps" ([H] -> [H]);
    CollOrdered = "coll_ordered" ([H, H] -> [H]);
    CollWhereWith = "coll_where_with" ([H, H, H] -> [H]);
    CollMoveTo = "coll_move_to" ([H, H, H, I64] -> []);
    Query = "query" ([H, H, H, H] -> [H]);
    Filters = "filters" ([H] -> [H]);
    FilterLit = "filter_lit" ([H, H, H] -> [H]);
    SortLit = "sort_lit" ([H, H] -> [H]);
    IdentPart = "ident_part" ([H, I32] -> [H]);
    RichNew = "rich_new" ([H] -> [H]);
    RichPart = "rich_part" ([H, I32] -> [H]);
    RichEmpty = "rich_empty" ([H] -> [I32]);
    RichTime = "rich_time" ([H] -> [H]);
    RichFromMarkdown = "rich_from_markdown" ([H] -> [H]);
    Phrase = "phrase" ([H, H] -> [H]);
    Word = "word" ([H, H, H] -> [H]);
    EntryParts = "entry_parts" ([H, H] -> [H]);
    Today = "today" ([] -> [H]);
    // The day a moment falls on and the time of day it is, both for the person; a day moved by
    // days; how far two days or two moments are apart; a part of a day, its weekday and the
    // day its week starts on; and a day or a moment a text names, read at a moment.
    TimeDay = "time_day" ([H] -> [H]);
    TimeOfDay = "time_of_day" ([H] -> [I64]);
    DayShift = "day_shift" ([H, I64, I32] -> [H]);
    TimeSpan = "time_span" ([H, H] -> [I64]);
    DayPart = "day_part" ([H, I32] -> [I64]);
    DayWeekday = "day_weekday" ([H] -> [I64]);
    DayStartOfWeek = "day_start_of_week" ([H] -> [H]);
    DayParse = "day_parse" ([H, H] -> [H]);
    TimeParse = "time_parse" ([H, H] -> [H]);
    // A draw (`uses random`): a number in `0..upto`, one call per number, which the invocation
    // records so a replay reads the same value.
    RandomInt = "random_int" ([I64, I64] -> [I64]);
    RandomFloat = "random_float" ([] -> [F64]);
    RandomChoice = "random_choice" ([H] -> [H]);
    RandomShuffle = "random_shuffle" ([H] -> [H]);
    RandomId = "random_id" ([] -> [H]);
    EventPart = "event_part" ([I32] -> [H]);
    CollMoveAmong = "coll_move_among" ([H, H, H, I64, H] -> []);
    UiActions = "ui_actions" ([H, H, H] -> []);
    UiAction = "ui_action" ([H, H, H] -> []);

    // A file: its name, media type or `tilo-blob://` address (by the part asked), its size in
    // bytes, its content read as text (a `Result`), and the chooser a control runs.
    FilePart = "file_part" ([H, I32] -> [H]);
    FileSize = "file_size" ([H] -> [I64]);
    FileText = "file_text" ([H] -> [H]);
    FileFiles = "file_files" ([H] -> [H]);
    ActChoose = "act_choose" ([H, H] -> [H]);
    ActSave = "act_save" ([H, H] -> [H]);

    SpaceI = "space_i" ([H] -> [I64]);
    SpaceF = "space_f" ([H] -> [F64]);

    Abort = "abort" ([H] -> []);
}
