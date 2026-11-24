//! The host imports, module `"tl"`: every function a compiled module may call, with its wasm signature.
//!
//! `Int` is an `i64`, `Float` an `f64`, `Bool` an `i32`. Every other value (text, lists, records,
//! references, actions, UI nodes) lives in the host and the guest holds an `i32` handle to it, so a
//! module can't forge a reference or an action: it can only name handles the host gave it. Fields,
//! types and actions are named by their ids, passed as string literal handles. An import traps on
//! misuse (a wrong kind of handle, a mutation outside an action); well-typed code never makes such a
//! call, apart from a reference literal that doesn't resolve. The runtime's table
//! (`tilolang_runtime::abi`) must match this one.

use crate::types::Vt;

macro_rules! imports {
    ($($v:ident = $n:literal ([$($p:ident),*] -> [$($r:ident),*]);)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Imp { $($v),* }

        impl Imp {
            pub const ALL: &'static [Self] = &[$(Self::$v),*];

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
    StrWords = "str_words" ([H] -> [I64]);
    IntToStr = "int_to_str" ([I64] -> [H]);
    StrToInt = "str_to_int" ([H] -> [H]);
    StrToFloat = "str_to_float" ([H] -> [H]);
    FloatToStr = "float_to_str" ([F64] -> [H]);
    DurationText = "duration_text" ([I64] -> [H]);
    StrCmp = "str_cmp" ([H, H] -> [I32]);
    StrFind = "str_find" ([H, H] -> [H]);
    StrSlice = "str_slice" ([H, I64, I64] -> [H]);
    StrReplace = "str_replace" ([H, H, H] -> [H]);
    StrUrlEncode = "str_url_encode" ([H] -> [H]);
    StrTrim = "str_trim" ([H] -> [H]);
    StrLower = "str_lower" ([H] -> [H]);
    StrUpper = "str_upper" ([H] -> [H]);
    StrStartsWith = "str_starts_with" ([H, H] -> [I32]);
    StrEndsWith = "str_ends_with" ([H, H] -> [I32]);
    StrContains = "str_contains" ([H, H] -> [I32]);
    StrSplit = "str_split" ([H, H] -> [H]);
    StrJoin = "str_join" ([H, H] -> [H]);
    StrMatches = "str_matches" ([H, H] -> [I32]);
    // The device's spelling: the words of a text no dictionary knows (`Text.misspelled`), and
    // what a word is most likely meant to be (`Text.suggestions`).
    StrMisspelled = "str_misspelled" ([H] -> [H]);
    StrSuggestions = "str_suggestions" ([H, H] -> [H]);
    // A text's digest, in lowercase hex: what a program keeps to compare text it must not
    // keep itself.
    Sha256 = "sha256" ([H] -> [H]);
    Blake3 = "blake3" ([H] -> [H]);

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
    // The elementary functions, in radians where an angle is one: a value asks its unit itself
    // with `to_radians` and `to_degrees`.
    FloatSqrt = "float_sqrt" ([F64] -> [F64]);
    FloatCbrt = "float_cbrt" ([F64] -> [F64]);
    FloatPow = "float_pow" ([F64, F64] -> [F64]);
    FloatExp = "float_exp" ([F64] -> [F64]);
    FloatLn = "float_ln" ([F64] -> [F64]);
    FloatLog10 = "float_log10" ([F64] -> [F64]);
    FloatLog2 = "float_log2" ([F64] -> [F64]);
    FloatSin = "float_sin" ([F64] -> [F64]);
    FloatCos = "float_cos" ([F64] -> [F64]);
    FloatTan = "float_tan" ([F64] -> [F64]);
    FloatAsin = "float_asin" ([F64] -> [F64]);
    FloatAcos = "float_acos" ([F64] -> [F64]);
    FloatAtan = "float_atan" ([F64] -> [F64]);
    FloatSinh = "float_sinh" ([F64] -> [F64]);
    FloatCosh = "float_cosh" ([F64] -> [F64]);
    FloatTanh = "float_tanh" ([F64] -> [F64]);
    FloatGamma = "float_gamma" ([F64] -> [F64]);
    FloatToRadians = "float_to_radians" ([F64] -> [F64]);
    FloatToDegrees = "float_to_degrees" ([F64] -> [F64]);
    FloatSign = "float_sign" ([F64] -> [F64]);
    FloatTrunc = "float_trunc" ([F64] -> [F64]);
    FloatFract = "float_fract" ([F64] -> [F64]);
    // Whether a number is one at all: a calculation that went past a float's range says so.
    FloatIsFinite = "float_is_finite" ([F64] -> [I32]);
    FloatIsNan = "float_is_nan" ([F64] -> [I32]);
    ListSumI = "list_sum_i" ([H] -> [I64]);
    ListSumF = "list_sum_f" ([H] -> [F64]);
    ListAvg = "list_avg" ([H] -> [F64]);

    ListNew = "list_new" ([] -> [H]);
    ListConcat = "list_concat" ([H, H] -> [H]);
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
    SomeB = "some_b" ([I32] -> [H]);
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
    CollInsertAs = "coll_insert_as" ([H, H, H, H] -> [H]);
    CollInsertKeyed = "coll_insert_keyed" ([H, H, H, H] -> [H]);
    CollRemove = "coll_remove" ([H, H, H] -> []);
    CollMove = "coll_move" ([H, H, H, H, I32] -> []);
    // A tree's order: the item and everything under it, to a place named by the parent field
    // and a neighbour (or the end of a parent's own).
    CollPlace = "coll_place" ([H, H, H, H, H, I32] -> []);
    CollWhere = "coll_where" ([H, H] -> [H]);
    CollRelatives = "coll_relatives" ([H, H, H, H, I32] -> [H]);
    CollCount = "coll_count" ([H] -> [I64]);
    CollCounted = "coll_counted" ([H] -> [H]);
    CollEmpty = "coll_empty" ([H] -> [I32]);
    // The texts the items of a collection hold in a field (by id), each once, in the order of the
    // alphabet: answered from the store's index.
    CollValues = "coll_values" ([H, H] -> [H]);
    SetAdd = "set_add" ([H, H, H] -> []);
    SetRemove = "set_remove" ([H, H, H] -> []);
    // A map held as a value (`let names: Map<Text> = Map<Text>()`), filled while the code runs.
    MapNew = "map_new" ([] -> [H]);
    MapPutValue = "map_put_value" ([H, H, H] -> []);
    MapRemoveValue = "map_remove_value" ([H, H] -> []);
    MapPut = "map_put" ([H, H, H, H] -> []);
    MapRemove = "map_remove" ([H, H, H] -> []);
    MapSetAdd = "map_set_add" ([H, H, H, H] -> []);
    MapSetRemove = "map_set_remove" ([H, H, H, H] -> []);
    MapGetH = "map_get_h" ([H, H, H] -> [H]);
    MapKeys = "map_keys" ([H, H] -> [H]);
    MapGetValue = "map_get_value" ([H, H] -> [H]);
    MapKeysValue = "map_keys_value" ([H] -> [H]);
    RecNew = "rec_new" ([H] -> [H]);
    RecordAt = "record_at" ([H, H] -> [H]);
    RecSetI = "rec_set_i" ([H, H, I64] -> []);
    RecSetF = "rec_set_f" ([H, H, F64] -> []);
    RecSetH = "rec_set_h" ([H, H, H] -> []);
    // `x.f = v` on a local: the value `x` holds, as one of its own, before it changes. A record
    // value is copied, so another name for it never sees the change; a stored resource is itself.
    RecOwn = "rec_own" ([H] -> [H]);
    RefLit = "ref_lit" ([H, H, H] -> [H]);
    // The reference to a root each device keeps its own (`device root readings: Readings`): the
    // same arguments as `ref_lit`, answered as this device's resource under `devices/`.
    DeviceRefLit = "device_ref_lit" ([H, H, H] -> [H]);
    RefEq = "ref_eq" ([H, H] -> [I32]);
    RefIs = "ref_is" ([H, H] -> [I32]);
    RefId = "ref_id" ([H] -> [H]);
    // A row's key: the id of a reference, or the row's place for a value a view made itself.
    RowKey = "row_key" ([H, I64] -> [H]);
    ResourceRef = "resource_ref" ([H] -> [H]);

    Invoker = "invoker" ([] -> [H]);
    MadeFrom = "made_from" ([] -> [H]);
    ActorEq = "actor_eq" ([H, H] -> [I32]);
    ActorName = "actor_name" ([H] -> [H]);
    ActorId = "actor_id" ([H] -> [H]);
    Now = "now" ([] -> [H]);
    PageCall = "page_call" ([H] -> [H]);
    Zones = "zones" ([] -> [H]);
    WebGet = "web_get" ([H, H, H] -> [H]);
    WebPost = "web_post" ([H, H, H] -> [H]);
    WebPostBytes = "web_post_bytes" ([H, I64, H] -> [H]);
    WebOk = "web_ok" ([H] -> [I32]);
    WebStatus = "web_status" ([H] -> [I64]);
    WebDurationMs = "web_duration_ms" ([H] -> [I64]);
    WebStatusText = "web_status_text" ([H] -> [H]);
    WebHeaders = "web_headers" ([H] -> [H]);
    WebBody = "web_body" ([H] -> [H]);
    WebText = "web_text" ([H] -> [H]);
    BytesLength = "bytes_length" ([H] -> [I64]);
    JsonParse = "json_parse" ([H] -> [H]);
    // The families this device sets text in: the ones the toolkit ships and the ones the
    // machine has installed, each once.
    FontFamilies = "font_families" ([] -> [H]);
    JsonDecode = "json_decode" ([H, H, H] -> [H]);
    JsonEncode = "json_encode" ([H] -> [H]);
    StageReading = "stage_reading" ([H, H] -> []);
    TimeShift = "time_shift" ([H, I64, I32] -> [H]);
    WallClock = "wall_clock" ([H] -> [H]);
    PersonNamed = "person_named" ([H] -> [H]);
    AgentNamed = "agent_named" ([H] -> [H]);
    InvocationPush = "invocation_push" ([H, H, H] -> []);
    InvocationPop = "invocation_pop" ([] -> []);
    ActorKind = "actor_kind" ([H] -> [I64]);
    ActorRole = "actor_role" ([H] -> [H]);
    ActorApp = "actor_app" ([H] -> [H]);

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
    Notify = "notify" ([H, H, H, H, H, H] -> []);
    Notice = "notice" ([H, H, I32] -> []);
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
    UiBindBegin = "ui_bind_begin" ([H, H] -> []);
    UiOn = "ui_on" ([H, H, H] -> []);
    UiAnchor = "ui_anchor" ([H, H] -> []);
    UiBindState = "ui_bind_state" ([H, H] -> []);
    UiBindStateAt = "ui_bind_state_at" ([H, H, I64] -> []);
    StateGet = "state_get" ([H, H] -> [H]);
    ActSet = "act_set" ([H, H] -> [H]);
    WindowState = "window_state" ([H, H, H] -> []);
    ActThen = "act_then" ([H, H] -> [H]);
    // `let id = act(args)` in a handler: `act`, and the steps after the binding, which the
    // runtime runs with the answer once `act` gives one.
    ActAnswer = "act_answer" ([H, H, H] -> [H]);
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
    // `call_members(r)`: who is in the system's call held on the resource `r` now, as its call
    // service hears it.
    CallMembers = "call_members" ([H] -> [H]);
    // `call_inputs(r)`: the microphones and cameras this device's call held on `r` can send,
    // which ones it sends from now, labelled as the person chooses between them.
    CallInputs = "call_inputs" ([H] -> [H]);
    // `join_call(r)` and its kin: the system's call held on `r` joined, left, or one of the
    // person's own switches in it (by name) turned on or off.
    ActCall = "act_call" ([H, H, I32] -> [H]);
    // `select_call_input(r, id)` and `set_call_volume(r, member, percent)`: which microphone or
    // camera this device sends in the call held on `r`, and how loud it plays one of its members.
    ActCallInput = "act_call_input" ([H, H] -> [H]);
    ActCallVolume = "act_call_volume" ([H, H, F64] -> [H]);
    // `invoked(id)`: the invocation the dispatch knows by `id`, as its record reads now.
    Invoked = "invoked" ([H] -> [H]);
    // `lines_run(r)`: the tilolang lines the person ran from this window in the conversation `r`.
    LinesRun = "lines_run" ([H] -> [H]);
    // `run_line(r, line)`: run `line` as the person's own step, from the conversation `r`.
    ActRunLine = "act_run_line" ([H, H] -> [H]);
    Delivery = "delivery" ([H] -> [H]);
    // `reach(r)`: how far this device's own changes of `r` have gone, as its sync keeps it.
    Reach = "reach" ([H] -> [H]);
    Members = "members" ([H] -> [H]);
    // `std.may_read(who, r)`: whether the member `who` may read the resource `r`.
    MayRead = "may_read" ([H, H] -> [I32]);
    Speakers = "speakers" ([H, I64] -> [H]);
    MetaOf = "meta_of" ([H] -> [H]);
    Versions = "versions" ([H] -> [H]);
    VersionChange = "version_change" ([H] -> [H]);
    Links = "links" ([H] -> [H]);
    Earlier = "earlier" ([H] -> [H]);
    VersionEnd = "version_end" ([H, I32] -> [H]);
    FieldChanged = "field_changed" ([H, H] -> [H]);
    RunChanges = "run_changes" ([H] -> [H]);
    Unseen = "unseen" ([H] -> [H]);
    Unreadable = "unreadable" ([H] -> [H]);
    FirstUnseen = "first_unseen" ([H, H, H] -> [H]);
    SeenBy = "seen_by" ([H, H] -> [H]);
    Mentions = "mentions" ([H, H, H] -> [I64]);
    Steps = "steps" ([H] -> [H]);
    // `ranked(items)`: the resources of a list, the ones the person looking chose most often and
    // most lately first, as their histories say.
    Ranked = "ranked" ([H] -> [H]);
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
    // `r.appended(other)`: the document with `other`'s paragraphs following as paragraphs of
    // their own, never running on into the last paragraph `RichText(a, b)` continues.
    RichAppended = "rich_appended" ([H, H] -> [H]);
    // `r.marked(name, value, at: anchor, start:, end:)`: the document with the mark `name` valued `value`
    // over the start-th to end-th character of the paragraph at `anchor` — a comment anchored to the
    // words it is about, written in the same change that makes the comment.
    RichMarked = "rich_marked" ([H, H, H, H, I64, I64] -> [H]);
    // `r.table_edited(table:, edit:, row:, column:)`: the document with its `table`th table (from 1)
    // changed by `edit` at a cell, only that table's paragraph rewritten.
    RichTableEdited = "rich_table_edited" ([H, I64, H, I64, I64] -> [H]);
    // `r.cell_written(table:, row:, column:, text:)`: the document with one table cell's words
    // rewritten and nothing else.
    RichCellWritten = "rich_cell_written" ([H, I64, I64, I64, H] -> [H]);
    // `r.replaced(find, with, all:)`: the document with `find` replaced in each paragraph's own
    // words, the marks around them kept.
    RichReplaced = "rich_replaced" ([H, H, H, I32] -> [H]);
    // The document's named styles: their names, one part of one's look, a part set, a style
    // copied under a new name, and a style deleted.
    RichStyleNames = "rich_style_names" ([H] -> [H]);
    RichStyleLook = "rich_style_look" ([H, H, H] -> [H]);
    RichStyleSet = "rich_style_set" ([H, H, H, H] -> [H]);
    RichStyleCopied = "rich_style_copied" ([H, H, H] -> [H]);
    RichStyleRemoved = "rich_style_removed" ([H, H] -> [H]);
    RichTime = "rich_time" ([H] -> [H]);
    // `RichText.from_markdown(text, links:, from:)`: the document a Markdown text says, a link to a page
    // of the bundle read as the reference to it. The arguments are the text, the pages by the address each
    // stands at, the folder the page is written in, and the app the references name.
    RichFromMarkdown = "rich_from_markdown" ([H, H, H, H] -> [H]);
    // `RichText.from_html(text)`: the document an HTML text says — what a message its sender
    // formatted says. Scripts and styles are not part of it, and a picture keeps the address it
    // names, so nothing outside the text is read.
    RichFromHtml = "rich_from_html" ([H] -> [H]);
    // `r.markdown(links:, from:)`: the document as Markdown, each reference to one of `links` written as
    // the link to the page it stands at, from the folder the page is written in.
    RichMarkdown = "rich_markdown" ([H, H, H] -> [H]);
    // `r.relinked(links:, sources:)`: the document with each link to a page of `links` the reference to
    // it, as `from_markdown` reads one, and each picture whose source `sources` names showing the source
    // it gives. The arguments are the document, the pages, the sources, and the app the references name.
    RichRelinked = "rich_relinked" ([H, H, H, H] -> [H]);
    RichEmbed = "rich_embed" ([H] -> [H]);
    Phrase = "phrase" ([H, H] -> [H]);
    Word = "word" ([H, H, H] -> [H]);
    EntryParts = "entry_parts" ([H, H] -> [H]);
    Today = "today" ([] -> [H]);
    // The day a moment falls on and the time of day it is, both for the person; a day moved by days; how
    // far two days or two moments are apart; a part of a day, its weekday and the day its week starts on;
    // and a day or a moment a text names, read at a moment.
    TimeDay = "time_day" ([H] -> [H]);
    // The day a moment falls on in a named zone: the same moment, read where people there are, from
    // the zone's own rules and never the person's clock.
    TimeDayIn = "time_day_in" ([H, H] -> [H]);
    // The moment a wall-clock time names in a named zone: the zone's own rules decide it, the
    // person's clock none of it.
    TimeFromWall = "time_from_wall" ([H, H] -> [H]);
    // The wall time a moment is in a named zone (`2026-11-24T09:00:00`), the form a calendar
    // stores; `floating`'s is where the reader is.
    TimeWall = "time_wall" ([H, H] -> [H]);
    TimeOfDay = "time_of_day" ([H] -> [I64]);
    TimeClock = "time_clock" ([H] -> [H]);
    DayShift = "day_shift" ([H, I64, I32] -> [H]);
    TimeSpan = "time_span" ([H, H] -> [I64]);
    Occurrences = "occurrences" ([H, H, H, H, H, H] -> [H]);
    DayPart = "day_part" ([H, I32] -> [I64]);
    DayWeekday = "day_weekday" ([H] -> [I64]);
    DayStartOfWeek = "day_start_of_week" ([H] -> [H]);
    DayParse = "day_parse" ([H, H] -> [H]);
    TimeParse = "time_parse" ([H, H] -> [H]);
    // A day (`I32` 0) or a moment (`I32` 1) as a sentence names it in the person's formats: `Jun 29`,
    // `Jun 29, 2025`, `Jun 29, 6:00 PM`, in the zone the person's device reads clocks in. Its stored
    // text where no formats are known.
    DateShow = "date_show" ([H, I32] -> [H]);
    // A draw (`uses random`): a number in `0..upto`, one call per number, which the invocation records so
    // a replay reads the same value.
    RandomInt = "random_int" ([I64, I64] -> [I64]);
    RandomFloat = "random_float" ([] -> [F64]);
    RandomChoice = "random_choice" ([H] -> [H]);
    RandomShuffle = "random_shuffle" ([H] -> [H]);
    RandomId = "random_id" ([] -> [H]);
    EventPart = "event_part" ([I32] -> [H]);
    // A press's index with the modifier keys held with it, as a `host.Press`.
    EventPress = "event_press" ([] -> [H]);
    CollMoveAmong = "coll_move_among" ([H, H, H, I64, H] -> []);
    UiActions = "ui_actions" ([H, H, H] -> []);
    UiAction = "ui_action" ([H, H, H] -> []);

    // A file: its name, media type or `tilo-blob://` address (by the part asked), its size in bytes, its
    // content read as text (a `Result`), and the chooser a control runs.
    FilePart = "file_part" ([H, I32] -> [H]);
    FileSize = "file_size" ([H] -> [I64]);
    FileText = "file_text" ([H] -> [H]);
    FileData = "file_data" ([H] -> [H]);
    FileFiles = "file_files" ([H] -> [H]);
    // A book the file is: its chapters, each beginning with its title as a heading; and what its
    // package says about it (title, author, language, subject).
    FileBook = "file_book" ([H] -> [H]);
    FileBookInfo = "file_book_info" ([H] -> [H]);
    FileBookCover = "file_book_cover" ([H] -> [H]);
    // A file an action makes of what it holds: the text, the bytes, or the content an address names, as a
    // file called by its name; and an archive of the files it holds, each under its own name.
    FileOfText = "file_of_text" ([H, H] -> [H]);
    FileOfBytes = "file_of_bytes" ([H, H] -> [H]);
    Base64Decode = "base64_decode" ([H] -> [H]);
    FileAt = "file_at" ([H, H] -> [H]);
    FileZip = "file_zip" ([H, H] -> [H]);
    // A document file read as the Markdown an editor shows, and Markdown written as a document
    // file of the format named, each a `Result` of its text or of the file.
    FileMarkdown = "file_markdown" ([H] -> [H]);
    FileOfDocument = "file_of_document" ([H, H, H] -> [H]);
    // A file's pixels: what its container says (an `Option` of the host's `ImageInfo`), and the
    // crop, turn, resize and encode a program applies, each a `Result` of the new file.
    FileImageInfo = "file_image_info" ([H] -> [H]);
    FileCrop = "file_crop" ([H, F64, F64, F64, F64] -> [H]);
    FileTurn = "file_turn" ([H, I64] -> [H]);
    FileResize = "file_resize" ([H, I64, I64] -> [H]);
    FileEncode = "file_encode" ([H, H, I64] -> [H]);
    // A file's preview, no larger than the size asked, from the device's preview cache (an
    // `Option` of the new file).
    FileThumbnail = "file_thumbnail" ([H, I64] -> [H]);
    // A spreadsheet file read as the sheets it holds, and a workbook written as a file of a
    // format: `.sheets()`, `File.of_sheets(name, workbook, format)`.
    SheetFile = "sheet_file" ([H] -> [H]);
    SheetOfFile = "sheet_of_file" ([H, H, H] -> [H]);
    // A workbook described as text (the JSON a cell's action builds) evaluated: what each of its
    // cells shows, in the workbook's own formats.
    SheetValues = "sheet_values" ([H] -> [H]);
    // The ranges of a sheet as a structural change leaves them.
    SheetRanges = "sheet_ranges" ([H, H, H] -> [H]);
    // The numbers a range of a sheet shows, row by row.
    SheetNumbers = "sheet_numbers" ([H, H, H] -> [H]);
    // The sheets a structural change leaves (rows or columns inserted or removed, a range sorted,
    // a fill extended, a sheet renamed), with the formulas the change moves rewritten.
    SheetEdit = "sheet_edit" ([H, H] -> [H]);
    // The cells a record action writes into a sheet: `set-records` by column header, or
    // `upsert-item` by the values that identify its row.
    SheetRecords = "sheet_records" ([H, H] -> [H]);
    // The input to store for a typed entry, and the format it shows in.
    SheetEntry = "sheet_entry" ([H, H] -> [H]);
    // The device's own files, within what the app was granted: a folder's entries, one entry,
    // the bytes a path holds, and the watch that runs an action when a folder changes.
    FileList = "file_list" ([H] -> [H]);
    FileStat = "file_stat" ([H] -> [H]);
    FileReadPath = "file_read_path" ([H] -> [H]);
    FileWatch = "file_watch" ([H, H] -> [H]);
    // A folder made, a file written, an entry renamed or moved or copied, one removed, one put
    // in this device's trash, the trash read and an entry restored from it.
    FileMkdir = "file_mkdir" ([H] -> [H]);
    FileWrite = "file_write" ([H, H] -> [H]);
    FileRename = "file_rename" ([H, H] -> [H]);
    FileMove = "file_move" ([H, H] -> [H]);
    FileCopy = "file_copy" ([H, H] -> [H]);
    FileRemove = "file_remove" ([H] -> [H]);
    FileTrash = "file_trash" ([H] -> [H]);
    FileTrashEntries = "file_trash_entries" ([] -> [H]);
    FileRestore = "file_restore" ([H] -> [H]);
    // Disk entries packed into an archive, and an archive extracted into a new folder.
    FileCompress = "file_compress" ([H, H] -> [H]);
    FileExtract = "file_extract" ([H, H] -> [H]);
    // The shell's tools an action asks of the device: the apps that open a file, opening it in
    // one of them, and a terminal at a folder.
    ToolDocumentApps = "tool_document_apps" ([H] -> [H]);
    ToolOpenWith = "tool_open_with" ([H, H] -> [H]);
    ToolTerminal = "tool_terminal" ([H] -> [H]);
    ToolOpenLink = "tool_open_link" ([H] -> [H]);
    // What the shell opened this app with, as JSON, or empty text: the folder a terminal
    // opened at, the file an app was asked to show.
    ToolContext = "tool_context" ([] -> [H]);
    ToolContainerShell = "tool_container_shell" ([H, H] -> [H]);
    // Opening a file in the app that opens its kind: the path the app was granted, or the file an
    // action holds, by its name and the address of its bytes.
    FileOpenPath = "file_open_path" ([H] -> [H]);
    FileOpen = "file_open" ([H] -> [H]);
    ActChoose = "act_choose" ([H, H] -> [H]);
    ActChooseFolder = "act_choose_folder" ([H, H] -> [H]);
    ActSave = "act_save" ([H, H] -> [H]);

    // A PDF, read by the device's engine: its pages, contents, a page's text and links, its
    // forms, the markup the file carries, one page as a picture, what the file holds as pages,
    // and a copy with the reader's markup in it. Each answer is JSON text the package reads into
    // its own records, and none of it is an effect: the bytes are all it reads.
    PdfPages = "pdf_pages" ([H] -> [H]);
    PdfOutline = "pdf_outline" ([H] -> [H]);
    PdfText = "pdf_text" ([H, I64] -> [H]);
    PdfLinks = "pdf_links" ([H, I64] -> [H]);
    PdfFields = "pdf_fields" ([H] -> [H]);
    PdfMarkup = "pdf_markup" ([H] -> [H]);
    PdfImport = "pdf_import" ([H] -> [H]);
    PdfPicture = "pdf_picture" ([H, I64, I64] -> [H]);
    PdfSave = "pdf_save" ([H, H] -> [H]);
    CanvasLook = "canvas_look" ([H, H, H] -> [H]);
    CanvasExport = "canvas_export" ([H, H, H] -> [H]);
    CanvasGrade = "canvas_grade" ([H, H] -> [H]);
    CanvasWand = "canvas_wand" ([H, H] -> [H]);
    CanvasFind = "canvas_find" ([H, H, H] -> [H]);
    CanvasCombine = "canvas_combine" ([H, H] -> [H]);
    CanvasMeasure = "canvas_measure" ([H, H, H] -> [H]);
    CanvasCompare = "canvas_compare" ([H, H, H] -> [H]);
    CanvasDiff = "canvas_diff" ([H, H, H, H] -> [H]);
    CanvasSelect = "canvas_select" ([H, H] -> [H]);
    CanvasInspect = "canvas_inspect" ([H, H] -> [H]);
    CanvasRemoveAlong = "canvas_remove_along" ([H, H] -> [H]);
    CanvasDiffPicture = "canvas_diff_picture" ([H, H, H, H] -> [H]);
    CanvasSvg = "canvas_svg" ([H, H] -> [H]);
    CanvasSvgImport = "canvas_svg_import" ([H, H] -> [H]);
    CanvasSave = "canvas_save" ([H, H] -> [H]);
    CanvasOpen = "canvas_open" ([H] -> [H]);

    // The clipboard: text or an image file offered as the person's copy, and the text or the image
    // on it now (an `Option`).
    ClipboardCopyText = "clipboard_copy_text" ([H] -> [I32]);
    ClipboardCopySecret = "clipboard_copy_secret" ([H] -> [I32]);
    ClipboardCopyImage = "clipboard_copy_image" ([H] -> [I32]);
    ClipboardPasteText = "clipboard_paste_text" ([] -> [H]);
    ClipboardPasteImage = "clipboard_paste_image" ([] -> [H]);

    SpaceI = "space_i" ([H] -> [I64]);
    SpaceF = "space_f" ([H] -> [F64]);
    // Whether the row the view is running for stands open into its document: what a slot that holds the
    // document is built under.
    RowOpen = "row_open" ([] -> [I32]);

    // The device's own work, asked by name: an answer now, a job the device records and runs
    // the actions it names as it moves (`done:`, `progress:`, each an `Option<Action>` the job
    // fills), a job's record as an action or a view reads it, a stream whose newest frame the
    // device keeps and the app shows by its address, what a stream says of itself as a view
    // reads it, or a picture the device keeps and the app shows by its address. The app asking
    // is the first argument, which the compiler fills.
    EngineCall = "engine_call" ([H, H, H] -> [H]);
    EngineStart = "engine_start" ([H, H, H, H, H] -> [H]);
    EngineJob = "engine_job" ([H] -> [H]);
    EngineStream = "engine_stream" ([H, H, H] -> [H]);
    EngineState = "engine_state" ([H] -> [H]);
    EnginePicture = "engine_picture" ([H, H, H] -> [H]);
    EngineCancel = "engine_cancel" ([H] -> [I32]);
    // A check the project's host approved, started by its id as a job `engine_status` and
    // `engine_cancel` follow; the app asking is the first argument.
    EngineCheck = "engine_check" ([H, H] -> [H]);

    // The code of this moment from a vault item's seed, read where the seed is: the item's
    // reference in, an `Option` of the code and its countdown out. The vault computes it and
    // writes nothing down, so a view may show it and no record holds it.
    VaultCode = "vault_code" ([H] -> [H]);

    // The app's held connections ([`crate::held`]): one opened by kind and key, a message sent to
    // it, what it last said of itself as a view reads it, and its end. The app opening one is the
    // first argument of an open, which the compiler fills.
    HeldOpen = "held_open" ([H, H, H, H] -> [H]);
    HeldSend = "held_send" ([H, H] -> [H]);
    HeldSaid = "held_said" ([H] -> [H]);
    HeldClose = "held_close" ([H] -> [I32]);

    Abort = "abort" ([H] -> []);
}
