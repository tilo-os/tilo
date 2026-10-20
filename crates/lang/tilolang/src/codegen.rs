//! The wasm backend: IR to the module shape of implementation.md section 4.2.

use crate::abi::Imp;
use crate::ir::{BinOp, Conv, Ek, Expr, ForList, Func, Module as IrModule, Stmt};
use crate::meta::META_SECTION;
use crate::types::Vt;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, DataSection, EntityType, ExportKind,
    ExportSection, Function, FunctionSection, ImportSection, Instruction, MemorySection,
    MemoryType, Module, TypeSection, ValType,
};

enum Op {
    I(Instruction<'static>),
    Imp(Imp),
    Func(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Label {
    Plain,
    ForBlock,
    ForLoop,
}

#[derive(Default)]
struct Pool {
    map: HashMap<String, (u32, u32)>,
    data: Vec<u8>,
}

impl Pool {
    fn intern(&mut self, s: &str) -> (u32, u32) {
        if let Some(v) = self.map.get(s) {
            return *v;
        }
        let ptr = self.data.len() as u32;
        self.data.extend_from_slice(s.as_bytes());
        let v = (ptr, s.len() as u32);
        self.map.insert(s.to_string(), v);
        v
    }
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
            Ek::Float(v) => self.i(Instruction::F64Const((*v).into())),
            Ek::Bool(b) => self.i(Instruction::I32Const(i32::from(*b))),
            Ek::I32(v) => self.i(Instruction::I32Const(*v)),
            Ek::Str(s) => {
                let (ptr, len) = self.pool.intern(s);
                self.i(Instruction::I32Const(ptr as i32));
                self.i(Instruction::I32Const(len as i32));
                self.ops.push(Op::Imp(Imp::StrLit));
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
        self.i(Instruction::LocalGet(fl.list_local));
        self.ops.push(Op::Imp(Imp::ListLen));
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

/// Emits the module. `meta_json`, when given, becomes the `tilolang.meta` custom section.
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
    let used: BTreeSet<Imp> = bodies
        .iter()
        .flatten()
        .filter_map(|op| if let Op::Imp(i) = op { Some(*i) } else { None })
        .collect();
    let import_index: HashMap<Imp, u32> = used
        .iter()
        .enumerate()
        .map(|(i, imp)| (*imp, i as u32))
        .collect();
    let n_imports = used.len() as u32;

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
    for t in &func_types {
        funcs.function(*t);
    }
    module.section(&funcs);
    let mut mem = MemorySection::new();
    let pages = (pool.data.len() as u64).div_ceil(65536).max(1);
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
        exports.export(&f.export, ExportKind::Func, n_imports + i as u32);
    }
    module.section(&exports);
    let mut code = CodeSection::new();
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
                Op::I(i) => func.instruction(i),
                Op::Imp(imp) => func.instruction(&Instruction::Call(
                    import_index.get(imp).copied().unwrap_or(0),
                )),
                Op::Func(f) => func.instruction(&Instruction::Call(n_imports + *f as u32)),
            };
        }
        code.function(&func);
    }
    module.section(&code);
    if !pool.data.is_empty() {
        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(0), pool.data.iter().copied());
        module.section(&data);
    }
    if let Some(json) = meta_json {
        module.section(&CustomSection {
            name: Cow::Borrowed(META_SECTION),
            data: Cow::Borrowed(json),
        });
    }
    module.finish()
}
