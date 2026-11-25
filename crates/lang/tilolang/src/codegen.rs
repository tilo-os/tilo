//! The wasm backend: IR to one wasm module per package.
//!
//! The module imports from `"tl"` only the [`Imp`]s it calls, in the order of that table. It exports
//! `memory`, which holds the string literals and nothing else, and one function per declaration, named by
//! its id: `a_<id>` for actions, `v_<id>` for views, `n_<id>` for fns, `d_<action>_<param>` for parameter
//! defaults and `b_main` for a block. The metadata goes in the custom section `tilolang.meta`. Suspending
//! and replaying a run is the host's job, so the guest keeps no state between calls.
//!
//! Literals are most used first in the data, so a hot literal's address and length are short immediates
//! at each of its uses. A literal whose uses save more than a table pair and the reader costs is read
//! through the module's own reader instead: a `(ptr, len)` pair at the end of the data, read by the
//! function just before the app's, which calls the runtime's `str_lit` with it, so a use is
//! `i32.const <slot>; call`. The runtime's ABI is unchanged and knows nothing of any of it.

use crate::abi::Imp;
use crate::ir::{BinOp, Conv, Ek, Expr, ForList, Func, Module as IrModule, Stmt};
use crate::meta::META_SECTION;
use crate::types::Vt;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, DataSection, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, ImportSection, Instruction, MemArg, MemorySection,
    MemoryType, Module, TypeSection, ValType,
};

enum Op {
    I(Instruction<'static>),
    Imp(Imp),
    Func(usize),
    /// A string literal, by its distinct index in the [`Pool`]: where it is, and whether a use
    /// writes it or reads it through the reader, is decided once the module's code is known.
    Str(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Label {
    Plain,
    ForBlock,
    ForLoop,
}

/// A table pair's bytes in the data: a literal's address and its length.
const PAIR: u32 = 8;

/// What the reader costs the module once, near enough to decide whether routing a literal through it
/// pays: its body, its local and its size in the code section.
const READER: u32 = 32;

#[derive(Default)]
struct Pool {
    map: HashMap<String, u32>,
    /// Each distinct string, in the order it is first used.
    strings: Vec<String>,
    /// How many uses each distinct string has.
    uses: Vec<u32>,
}

/// The data the module carries, and which literals its reader answers.
struct Layout {
    data: Vec<u8>,
    /// Where each distinct literal is, by its first use.
    at: Vec<(u32, u32)>,
    /// Where the reader's table is in the data.
    table: u32,
    /// The table slot of each distinct literal, for the ones it holds.
    slots: Vec<Option<u32>>,
}

impl Pool {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.map.get(s) {
            self.uses[*i as usize] += 1;
            return *i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        self.uses.push(1);
        self.map.insert(s.to_owned(), i);
        i
    }

    /// The data, and which literals the reader at `reader` answers.
    ///
    /// The literals go most used first, and a tie keeps the order they are first used in, so the same
    /// source always gives the same bytes. Then each literal, in that same order, is routed through
    /// the reader when its uses save more than its table pair and, once, the reader's own bytes: its
    /// slot is its place among the routed, so the hottest slots are the cheapest immediates.
    fn layout(&self, str_lit: u32, reader: u32) -> Layout {
        let mut order: Vec<u32> = (0..self.strings.len() as u32).collect();
        order.sort_by(|a, b| {
            self.uses[*b as usize]
                .cmp(&self.uses[*a as usize])
                .then(a.cmp(b))
        });
        let mut data = Vec::new();
        let mut at = vec![(0, 0); self.strings.len()];
        for &i in &order {
            let s = &self.strings[i as usize];
            at[i as usize] = (data.len() as u32, s.len() as u32);
            data.extend_from_slice(s.as_bytes());
        }
        let table = data.len() as u32;
        let mut slots = vec![None; self.strings.len()];
        let mut routed = 0;
        for &i in &order {
            let (ptr, len) = at[i as usize];
            let direct = const_bytes(ptr) + const_bytes(len) + call_bytes(str_lit);
            let through = const_bytes(routed) + call_bytes(reader);
            let once = if routed == 0 { READER } else { 0 };
            if direct <= through || (direct - through) * self.uses[i as usize] <= PAIR + once {
                continue;
            }
            slots[i as usize] = Some(routed);
            data.extend_from_slice(&ptr.to_le_bytes());
            data.extend_from_slice(&len.to_le_bytes());
            routed += 1;
        }
        Layout {
            data,
            at,
            table,
            slots,
        }
    }
}

/// The bytes an `i32.const` takes.
fn const_bytes(v: u32) -> u32 {
    1 + sleb_bytes(v.into())
}

/// The bytes a `call` takes, its index included.
fn call_bytes(index: u32) -> u32 {
    1 + uleb_bytes(index.into())
}

/// The bytes a signed LEB128 takes.
fn sleb_bytes(mut v: i64) -> u32 {
    let mut bytes = 1;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0) {
            return bytes;
        }
        bytes += 1;
    }
}

/// The bytes an unsigned LEB128 takes.
fn uleb_bytes(mut v: u64) -> u32 {
    let mut bytes = 1;
    while v >= 0x80 {
        v >>= 7;
        bytes += 1;
    }
    bytes
}

const fn val(v: Vt) -> ValType {
    match v {
        Vt::I32 => ValType::I32,
        Vt::I64 => ValType::I64,
        Vt::F64 => ValType::F64,
    }
}

fn block_type(ty: &crate::types::Ty) -> BlockType {
    ty.repr()
        .map_or(BlockType::Empty, |v| BlockType::Result(val(v)))
}

struct Gen<'a> {
    ops: Vec<Op>,
    labels: Vec<Label>,
    pool: &'a mut Pool,
}

impl Gen<'_> {
    fn i(&mut self, i: Instruction<'static>) {
        self.ops.push(Op::I(i));
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            Ek::Int(v) => self.i(Instruction::I64Const(*v)),
            Ek::Duration(secs) => self.i(Instruction::I64Const(*secs)),
            Ek::Float(v) => self.i(Instruction::F64Const((*v).into())),
            Ek::Bool(b) => self.i(Instruction::I32Const(i32::from(*b))),
            Ek::I32(v) => self.i(Instruction::I32Const(*v)),
            Ek::Str(s) => {
                let i = self.pool.intern(s);
                self.ops.push(Op::Str(i));
            }
            Ek::Local(l) => self.i(Instruction::LocalGet(*l)),
            Ek::Host(imp, args) => {
                for a in args {
                    self.expr(a);
                }
                self.ops.push(Op::Imp(*imp));
                if !imp.results().is_empty() && e.ty.repr().is_none() {
                    self.i(Instruction::Drop);
                }
            }
            Ek::Call(f, args) => {
                for a in args {
                    self.expr(a);
                }
                self.ops.push(Op::Func(*f));
            }
            Ek::Bin(op, l, r) => {
                self.expr(l);
                self.expr(r);
                let inst = bin_instr(*op, l.ty.repr());
                self.i(inst);
            }
            Ek::Neg(x) => {
                if x.ty.repr() == Some(Vt::F64) {
                    self.expr(x);
                    self.i(Instruction::F64Neg);
                } else {
                    self.i(Instruction::I64Const(0));
                    self.expr(x);
                    self.i(Instruction::I64Sub);
                }
            }
            Ek::Not(x) => {
                self.expr(x);
                self.i(Instruction::I32Eqz);
            }
            Ek::If(c, t, f) => {
                self.expr(c);
                self.i(Instruction::If(block_type(&e.ty)));
                self.labels.push(Label::Plain);
                self.expr(t);
                self.i(Instruction::Else);
                self.expr(f);
                self.i(Instruction::End);
                self.labels.pop();
            }
            Ek::Seq(stmts, v) => {
                self.stmts(stmts);
                self.expr(v);
            }
            Ek::Unit => {}
            Ek::Unreachable => self.i(Instruction::Unreachable),
            Ek::Conv(c, x) => {
                self.expr(x);
                match c {
                    Conv::BoolToInt => self.i(Instruction::I64ExtendI32U),
                    Conv::SignExt => self.i(Instruction::I64ExtendI32S),
                    Conv::IntToFloat => self.i(Instruction::F64ConvertI64S),
                    Conv::FloatToInt => self.i(Instruction::I64TruncSatF64S),
                    Conv::IntToBool => {
                        self.i(Instruction::I64Const(0));
                        self.i(Instruction::I64Ne);
                    }
                }
            }
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Set(l, e) => {
                self.expr(e);
                self.i(Instruction::LocalSet(*l));
            }
            Stmt::Eval(e) => {
                self.expr(e);
                if e.ty.repr().is_some() {
                    self.i(Instruction::Drop);
                }
            }
            Stmt::If(c, t, f) => {
                self.expr(c);
                self.i(Instruction::If(BlockType::Empty));
                self.labels.push(Label::Plain);
                self.stmts(t);
                if !f.is_empty() {
                    self.i(Instruction::Else);
                    self.stmts(f);
                }
                self.i(Instruction::End);
                self.labels.pop();
            }
            Stmt::ForList(fl) => self.for_list(fl),
            Stmt::Break => match self.labels.iter().rev().position(|l| *l == Label::ForBlock) {
                Some(depth) => self.i(Instruction::Br(depth as u32)),
                None => self.i(Instruction::Unreachable),
            },
            Stmt::Return(v) => {
                if let Some(e) = v {
                    self.expr(e);
                }
                self.i(Instruction::Return);
            }
        }
    }

    fn for_list(&mut self, fl: &ForList) {
        self.expr(&fl.list);
        self.i(Instruction::LocalSet(fl.list_local));
        self.i(Instruction::I64Const(0));
        self.i(Instruction::LocalSet(fl.idx_local));
        // A loop reads every item, so it takes the count, not the length the store knows without reading it:
        // a collection that checks its items as it reads them knows only how many it can hold at most until it
        // has read them, and a loop bounded by that reads past the end ([`Imp::CollCount`]).
        self.i(Instruction::LocalGet(fl.list_local));
        self.ops.push(Op::Imp(Imp::CollCount));
        self.i(Instruction::LocalSet(fl.len_local));
        self.i(Instruction::Block(BlockType::Empty));
        self.labels.push(Label::ForBlock);
        self.i(Instruction::Loop(BlockType::Empty));
        self.labels.push(Label::ForLoop);
        self.i(Instruction::LocalGet(fl.idx_local));
        self.i(Instruction::LocalGet(fl.len_local));
        self.i(Instruction::I64GeS);
        self.i(Instruction::BrIf(1));
        self.i(Instruction::LocalGet(fl.list_local));
        self.i(Instruction::LocalGet(fl.idx_local));
        self.ops.push(Op::Imp(fl.get));
        if fl.get_conv == Some(Conv::IntToBool) {
            self.i(Instruction::I64Const(0));
            self.i(Instruction::I64Ne);
        }
        self.i(Instruction::LocalSet(fl.item_local));
        self.stmts(&fl.body);
        self.i(Instruction::LocalGet(fl.idx_local));
        self.i(Instruction::I64Const(1));
        self.i(Instruction::I64Add);
        self.i(Instruction::LocalSet(fl.idx_local));
        self.i(Instruction::Br(0));
        self.i(Instruction::End);
        self.labels.pop();
        self.i(Instruction::End);
        self.labels.pop();
    }
}

fn bin_instr(op: BinOp, vt: Option<Vt>) -> Instruction<'static> {
    match (vt, op) {
        (Some(Vt::I64), BinOp::Add) => Instruction::I64Add,
        (Some(Vt::I64), BinOp::Sub) => Instruction::I64Sub,
        (Some(Vt::I64), BinOp::Mul) => Instruction::I64Mul,
        (Some(Vt::I64), BinOp::Div) => Instruction::I64DivS,
        (Some(Vt::I64), BinOp::Rem) => Instruction::I64RemS,
        (Some(Vt::I64), BinOp::Eq) => Instruction::I64Eq,
        (Some(Vt::I64), BinOp::Ne) => Instruction::I64Ne,
        (Some(Vt::I64), BinOp::Lt) => Instruction::I64LtS,
        (Some(Vt::I64), BinOp::Le) => Instruction::I64LeS,
        (Some(Vt::I64), BinOp::Gt) => Instruction::I64GtS,
        (Some(Vt::I64), BinOp::Ge) => Instruction::I64GeS,
        (Some(Vt::F64), BinOp::Add) => Instruction::F64Add,
        (Some(Vt::F64), BinOp::Sub) => Instruction::F64Sub,
        (Some(Vt::F64), BinOp::Mul) => Instruction::F64Mul,
        (Some(Vt::F64), BinOp::Div) => Instruction::F64Div,
        (Some(Vt::F64), BinOp::Eq) => Instruction::F64Eq,
        (Some(Vt::F64), BinOp::Ne) => Instruction::F64Ne,
        (Some(Vt::F64), BinOp::Lt) => Instruction::F64Lt,
        (Some(Vt::F64), BinOp::Le) => Instruction::F64Le,
        (Some(Vt::F64), BinOp::Gt) => Instruction::F64Gt,
        (Some(Vt::F64), BinOp::Ge) => Instruction::F64Ge,
        (Some(Vt::I32), BinOp::Eq) => Instruction::I32Eq,
        (Some(Vt::I32), BinOp::Ne) => Instruction::I32Ne,
        _ => Instruction::Unreachable,
    }
}

fn func_sig(f: &Func) -> (Vec<ValType>, Vec<ValType>) {
    let params = f
        .locals
        .iter()
        .take(f.nparams)
        .map(|t| val(t.repr().unwrap_or(Vt::I32)))
        .collect();
    let results = f.result.repr().map(val).into_iter().collect();
    (params, results)
}

pub fn emit(m: &IrModule, meta_json: Option<&[u8]>) -> Vec<u8> {
    let mut pool = Pool::default();
    let bodies: Vec<Vec<Op>> = m
        .funcs
        .iter()
        .map(|f| {
            let mut g = Gen {
                ops: Vec::new(),
                labels: Vec::new(),
                pool: &mut pool,
            };
            g.stmts(&f.body);
            if f.result.repr().is_some() {
                g.i(Instruction::Unreachable);
            }
            g.i(Instruction::End);
            g.ops
        })
        .collect();
    let mut used: BTreeSet<Imp> = bodies
        .iter()
        .flatten()
        .filter_map(|op| if let Op::Imp(i) = op { Some(*i) } else { None })
        .collect();
    if !pool.strings.is_empty() {
        // The reader calls `str_lit` itself, whether a use does.
        used.insert(Imp::StrLit);
    }
    let import_index: HashMap<Imp, u32> = used
        .iter()
        .enumerate()
        .map(|(i, imp)| (*imp, i as u32))
        .collect();
    let n_imports = used.len() as u32;
    let str_lit = import_index.get(&Imp::StrLit).copied().unwrap_or_default();
    // The reader comes before the app's functions, so a use's call names it in one byte where the
    // import table is short; a literal use is far more common than a call between two of the app's
    // functions, which moves by at most a byte at a LEB boundary.
    let layout = pool.layout(str_lit, n_imports);
    let reader = layout.slots.iter().any(Option::is_some);
    let shift = u32::from(reader);

    let mut sigs: Vec<(Vec<ValType>, Vec<ValType>)> = Vec::new();
    let mut type_of = |p: Vec<ValType>, r: Vec<ValType>| -> u32 {
        if let Some(i) = sigs.iter().position(|(a, b)| *a == p && *b == r) {
            return i as u32;
        }
        sigs.push((p, r));
        (sigs.len() - 1) as u32
    };
    let import_types: Vec<u32> = used
        .iter()
        .map(|imp| {
            type_of(
                imp.params().iter().copied().map(val).collect(),
                imp.results().iter().copied().map(val).collect(),
            )
        })
        .collect();
    let reader_type = reader.then(|| type_of(vec![ValType::I32], vec![ValType::I32]));
    let func_types: Vec<u32> = m
        .funcs
        .iter()
        .map(|f| {
            let (p, r) = func_sig(f);
            type_of(p, r)
        })
        .collect();

    let mut module = Module::new();
    let mut types = TypeSection::new();
    for (p, r) in &sigs {
        types.ty().function(p.iter().copied(), r.iter().copied());
    }
    module.section(&types);
    if !used.is_empty() {
        let mut imports = ImportSection::new();
        for (imp, ty) in used.iter().zip(&import_types) {
            imports.import("tl", imp.name(), EntityType::Function(*ty));
        }
        module.section(&imports);
    }
    let mut funcs = FunctionSection::new();
    if let Some(ty) = reader_type {
        funcs.function(ty);
    }
    for t in &func_types {
        funcs.function(*t);
    }
    module.section(&funcs);
    let mut mem = MemorySection::new();
    let pages = (layout.data.len() as u64).div_ceil(65536).max(1);
    mem.memory(MemoryType {
        minimum: pages,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&mem);
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    for (i, f) in m.funcs.iter().enumerate() {
        exports.export(&f.export, ExportKind::Func, n_imports + shift + i as u32);
    }
    module.section(&exports);
    let mut code = CodeSection::new();
    if reader {
        let mut func = Function::new_with_locals_types([ValType::I32]);
        read_literal(&mut func, layout.table, str_lit);
        code.function(&func);
    }
    for (f, ops) in m.funcs.iter().zip(&bodies) {
        let locals: Vec<ValType> = f
            .locals
            .iter()
            .skip(f.nparams)
            .map(|t| val(t.repr().unwrap_or(Vt::I32)))
            .collect();
        let mut func = Function::new_with_locals_types(locals);
        for op in ops {
            match op {
                Op::I(i) => {
                    func.instruction(i);
                }
                Op::Imp(imp) => {
                    func.instruction(&Instruction::Call(
                        import_index.get(imp).copied().unwrap_or(0),
                    ));
                }
                Op::Func(f) => {
                    func.instruction(&Instruction::Call(n_imports + shift + *f as u32));
                }
                Op::Str(i) => match layout.slots[*i as usize] {
                    Some(slot) => {
                        func.instruction(&Instruction::I32Const(slot as i32));
                        func.instruction(&Instruction::Call(n_imports));
                    }
                    None => {
                        let (ptr, len) = layout.at[*i as usize];
                        func.instruction(&Instruction::I32Const(ptr as i32));
                        func.instruction(&Instruction::I32Const(len as i32));
                        func.instruction(&Instruction::Call(str_lit));
                    }
                },
            }
        }
        code.function(&func);
    }
    module.section(&code);
    if !layout.data.is_empty() {
        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(0), layout.data.iter().copied());
        module.section(&data);
    }
    if let Some(json) = meta_json {
        module.section(&CustomSection {
            name: Cow::Borrowed(META_SECTION),
            data: Cow::Borrowed(json),
        });
    }
    checked(module.finish())
}

/// The reader's body: the `(ptr, len)` pair at `table + slot * 8`, handed to the runtime's
/// `str_lit`, which answers the literal's handle.
fn read_literal(func: &mut Function, table: u32, str_lit: u32) {
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Const(PAIR as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const(table as i32));
    func.instruction(&Instruction::I32Add);
    // Local 1 holds the pair's address: its first four bytes are the literal's address, the next
    // four its length.
    func.instruction(&Instruction::LocalSet(1));
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::I32Load(MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }));
    func.instruction(&Instruction::Call(str_lit));
    func.instruction(&Instruction::End);
}

/// The module, validated before the compiler returns it.
///
/// A host refuses a module that does not validate, and the reason is one the compiler made: a type
/// the checker gives an expression, the import table gives a host call and the function signature
/// gives a local must agree, and nothing else reads the bytes back. `tilolang check` compiles the
/// module but never runs it, so without this a module the host cannot load reaches the device.
fn checked(module: Vec<u8>) -> Vec<u8> {
    let Err(error) = wasmparser::Validator::new().validate_all(&module) else {
        return module;
    };
    let at = function_at(&module, error.offset())
        .map_or_else(String::new, |export| format!(", in `{export}`"));
    fail(&format!(
        "the compiler emitted a wasm module its host would refuse{at}: {error}"
    ))
}

#[expect(
    clippy::panic,
    reason = "an invalid module is a compiler bug with no source line to report and no correct \
              module to hand back: failing where it is made names it, and the host would refuse \
              the module anyway"
)]
fn fail(message: &str) -> ! {
    panic!("{message}")
}

/// The export of the function whose body holds `offset`, for a validator that names bytes alone.
fn function_at(wasm: &[u8], offset: usize) -> Option<String> {
    use wasmparser::{ExternalKind, Parser, Payload, TypeRef};
    let mut imported = 0u32;
    let mut names: HashMap<u32, &str> = HashMap::new();
    let mut bodies = 0u32;
    let mut held = None;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.ok()? {
            Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    if matches!(import.ok()?.ty, TypeRef::Func(_)) {
                        imported = imported.saturating_add(1);
                    }
                }
            }
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.ok()?;
                    if export.kind == ExternalKind::Func {
                        names.insert(export.index, export.name);
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                let range = body.range();
                if range.start <= offset && offset < range.end {
                    held = names
                        .get(&imported.saturating_add(bodies))
                        .map(|name| (*name).to_owned());
                }
                bodies = bodies.saturating_add(1);
            }
            _ => {}
        }
    }
    held
}
