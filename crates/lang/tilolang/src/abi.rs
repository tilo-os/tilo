//! The host imports of implementation.md section 4.4, module `"tl"`.

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

    TupleNew = "tuple_new" ([I32] -> [H]);
    TupleSetI = "tuple_set_i" ([H, I32, I64] -> []);
    TupleSetF = "tuple_set_f" ([H, I32, F64] -> []);
    TupleSetH = "tuple_set_h" ([H, I32, H] -> []);

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
    CollMove = "coll_move" ([H, H, H, I64] -> []);
    SetAdd = "set_add" ([H, H, H] -> []);
    SetRemove = "set_remove" ([H, H, H] -> []);
    MapPut = "map_put" ([H, H, H, H] -> []);
    MapRemove = "map_remove" ([H, H, H] -> []);
    RecNew = "rec_new" ([H] -> [H]);
    RecSetI = "rec_set_i" ([H, H, I64] -> []);
    RecSetF = "rec_set_f" ([H, H, F64] -> []);
    RecSetH = "rec_set_h" ([H, H, H] -> []);
    RefLit = "ref_lit" ([H, H, H] -> [H]);
    RefEq = "ref_eq" ([H, H] -> [I32]);
    RefId = "ref_id" ([H] -> [H]);

    ActRef = "act_ref" ([H, H] -> [H]);
    Invoke = "invoke" ([H] -> [H]);
    Outside = "outside" ([H, H] -> [H]);
    ModelCall = "model_call" ([H, H] -> [H]);

    UiNode = "ui_node" ([H, H] -> [H]);
    UiPropH = "ui_prop_h" ([H, H, H] -> []);
    UiPropI = "ui_prop_i" ([H, H, I64] -> []);
    UiPropF = "ui_prop_f" ([H, H, F64] -> []);
    UiChild = "ui_child" ([H, H] -> []);
    UiBind = "ui_bind" ([H, H, H] -> []);
    UiOn = "ui_on" ([H, H, H] -> []);
    UiAnchor = "ui_anchor" ([H, H] -> []);

    SpaceI = "space_i" ([H] -> [I64]);
    SpaceF = "space_f" ([H] -> [F64]);

    Abort = "abort" ([H] -> []);
}
