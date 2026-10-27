//! Merge the copies of a function a module carries more than once.
//!
//! An `#[inline]` function gets a local copy in every crate that calls it,
//! and fat LTO keeps them apart: the start module held dozens of copies of
//! `serde_json::Value::clone` and its kin. wasm-opt merges a copy only when
//! its body, callees included, is identical to another's, so copies that
//! call each other (a `clone` that recurses through its own `Vec` clone)
//! each keep a cycle of their own and are never merged.
//!
//! Two functions are equivalent when they have the same type and the same
//! body, byte for byte, except for the functions they call, and those are
//! equivalent in turn. Equivalence is found by partition refinement, as a
//! finite automaton is minimised: functions start grouped by type and body
//! with callees left out, and a group is split until all its members call
//! into the same groups at the same places. Every reference to a function
//! (calls, `ref.func`, tables, exports, globals, the start function) is then
//! pointed at the first function of its group, and the copies nothing refers
//! to any more are dropped.
//!
//! This is sound whatever the copies are named: equivalent functions run the
//! same instructions on equivalent callees, so each computes what the other
//! does. A body that differs in anything else (a constant, a static's
//! address, a function pointer) keeps its own group.

use std::collections::HashMap;

use anyhow::{Context, Result};
use walrus::ir::VisitorMut;
use walrus::{ConstExpr, ElementItems, ExportItem, FunctionId, FunctionKind, GlobalKind};

/// `bytes` with every function equivalent to an earlier one replaced by it,
/// and how many functions that left out. `bytes` itself when there are none.
pub fn merge_duplicates(bytes: &[u8]) -> Result<(Vec<u8>, usize)> {
    let representative = representatives(bytes)?;
    let merged = representative
        .iter()
        .enumerate()
        .filter(|(index, rep)| *index != **rep as usize)
        .count();
    if merged == 0 {
        return Ok((bytes.to_vec(), 0));
    }
    let mut module = walrus::ModuleConfig::new()
        .generate_producers_section(false)
        .parse(bytes)
        .context("parsing the module to merge its duplicate functions")?;
    // walrus keeps functions in index order: imports, then the code section.
    let ids: Vec<FunctionId> = module.funcs.iter().map(|function| function.id()).collect();
    anyhow::ensure!(
        ids.len() == representative.len(),
        "walrus found {} functions where the code section has {}",
        ids.len(),
        representative.len()
    );
    let to: HashMap<FunctionId, FunctionId> = representative
        .iter()
        .enumerate()
        .filter(|(index, rep)| *index != **rep as usize)
        .map(|(index, rep)| (ids[index], ids[*rep as usize]))
        .collect();
    let mut redirect = Redirect(&to);
    let point = |id: &mut FunctionId| {
        if let Some(rep) = to.get(id) {
            *id = *rep;
        }
    };
    for function in module.funcs.iter_mut() {
        if let FunctionKind::Local(local) = &mut function.kind {
            let entry = local.entry_block();
            walrus::ir::dfs_pre_order_mut(&mut redirect, local, entry);
        }
    }
    for element in module.elements.iter_mut() {
        match &mut element.items {
            ElementItems::Functions(functions) => functions.iter_mut().for_each(point),
            ElementItems::Expressions(_, exprs) => {
                for expr in exprs {
                    if let ConstExpr::RefFunc(id) = expr {
                        point(id);
                    }
                }
            }
        }
    }
    for export in module.exports.iter_mut() {
        if let ExportItem::Function(id) = &mut export.item {
            point(id);
        }
    }
    let globals: Vec<_> = module.globals.iter().map(|global| global.id()).collect();
    for global in globals {
        if let GlobalKind::Local(ConstExpr::RefFunc(id)) = &mut module.globals.get_mut(global).kind
        {
            point(id);
        }
    }
    if let Some(start) = &mut module.start {
        point(start);
    }
    walrus::passes::gc::run(&mut module);
    Ok((module.emit_wasm(), merged))
}

/// Points every reference to a function at its group's first function.
struct Redirect<'a>(&'a HashMap<FunctionId, FunctionId>);

impl VisitorMut for Redirect<'_> {
    fn visit_function_id_mut(&mut self, function: &mut FunctionId) {
        if let Some(rep) = self.0.get(function) {
            *function = *rep;
        }
    }
}

/// For every function index, the first index of its group of equivalent
/// functions. Imported functions are each their own group.
pub fn representatives(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut imported = 0_u32;
    let mut types: Vec<u32> = Vec::new();
    // Each local function's body with the function indices it refers to
    // left out, and those indices in order.
    let mut bodies: Vec<(Vec<u8>, Vec<u32>)> = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload? {
            wasmparser::Payload::ImportSection(imports) => {
                for import in imports {
                    if matches!(import?.ty, wasmparser::TypeRef::Func(_)) {
                        imported += 1;
                    }
                }
            }
            wasmparser::Payload::FunctionSection(functions) => {
                for ty in functions {
                    types.push(ty?);
                }
            }
            wasmparser::Payload::CodeSectionEntry(body) => bodies.push(shape(bytes, &body)?),
            _ => {}
        }
    }
    anyhow::ensure!(
        types.len() == bodies.len(),
        "{} function types for {} bodies",
        types.len(),
        bodies.len()
    );
    let count = imported as usize + bodies.len();
    // The first partition: imports alone, local functions by type and body.
    let mut group: Vec<u32> = Vec::with_capacity(count);
    let mut groups = 0_u32;
    group.extend((0..imported).inspect(|_| groups += 1));
    {
        let mut first: HashMap<(u32, &[u8]), u32> = HashMap::new();
        for (ty, (body, _)) in types.iter().zip(&bodies) {
            let id = *first.entry((*ty, body.as_slice())).or_insert_with(|| {
                groups += 1;
                groups - 1
            });
            group.push(id);
        }
    }
    // Split groups by the groups their callees are in until none splits.
    loop {
        let mut next: Vec<u32> = Vec::with_capacity(count);
        let mut split: HashMap<(u32, Vec<u32>), u32> = HashMap::new();
        let mut next_groups = 0_u32;
        for index in 0..count {
            let callees = match index.checked_sub(imported as usize) {
                Some(local) => bodies[local]
                    .1
                    .iter()
                    .map(|callee| group.get(*callee as usize).copied().unwrap_or(u32::MAX))
                    .collect(),
                // An import refers to nothing; it is alone in its group.
                None => vec![index as u32],
            };
            let id = *split.entry((group[index], callees)).or_insert_with(|| {
                next_groups += 1;
                next_groups - 1
            });
            next.push(id);
        }
        let stable = next_groups == groups;
        group = next;
        groups = next_groups;
        if stable {
            break;
        }
    }
    let mut first_of: HashMap<u32, u32> = HashMap::new();
    Ok(group
        .iter()
        .enumerate()
        .map(|(index, id)| *first_of.entry(*id).or_insert(index as u32))
        .collect())
}

/// A function body's bytes, locals included, with every function index it
/// refers to (`call`, `return_call`, `ref.func`) replaced by one marker
/// byte, and those indices in order.
fn shape(bytes: &[u8], body: &wasmparser::FunctionBody<'_>) -> Result<(Vec<u8>, Vec<u32>)> {
    let range = body.range();
    let mut shape = Vec::with_capacity(range.len());
    let mut callees = Vec::new();
    let mut copied = range.start;
    let mut operators = body.get_operators_reader()?;
    while !operators.eof() {
        let (operator, offset) = operators.read_with_offset()?;
        let callee = match operator {
            wasmparser::Operator::Call { function_index }
            | wasmparser::Operator::ReturnCall { function_index }
            | wasmparser::Operator::RefFunc { function_index } => function_index,
            _ => continue,
        };
        // Each of these is a one-byte opcode and the index's LEB128.
        let immediate = offset + 1;
        let mut end = immediate;
        while bytes
            .get(end)
            .context("a function index runs past the body")?
            & 0x80
            != 0
        {
            end += 1;
        }
        shape.extend_from_slice(&bytes[copied..immediate]);
        shape.push(0xff);
        callees.push(callee);
        copied = end + 1;
    }
    shape.extend_from_slice(&bytes[copied..range.end]);
    Ok((shape, callees))
}

#[cfg(test)]
mod tests {
    use super::*;
    use walrus::ir::BinaryOp;
    use walrus::{FunctionBuilder, Module, ModuleConfig, ValType};

    /// A module exporting `a` and `b`, two copies of a function that counts
    /// `n` down by calling itself and adds 1 per call, and `c`, which adds 3.
    fn counters() -> Vec<u8> {
        let mut module = Module::with_config(ModuleConfig::new());
        let ty = module.types.add(&[ValType::I32], &[ValType::I32]);
        let (placeholder, _) = module.add_import_func("env", "placeholder", ty);
        for (name, step) in [("a", 1), ("b", 1), ("c", 3)] {
            let mut builder =
                FunctionBuilder::new(&mut module.types, &[ValType::I32], &[ValType::I32]);
            let n = module.locals.add(ValType::I32);
            builder.func_body().local_get(n).if_else(
                ValType::I32,
                |then| {
                    then.i32_const(0);
                },
                |otherwise| {
                    otherwise
                        .local_get(n)
                        .i32_const(1)
                        .binop(BinaryOp::I32Sub)
                        .call(placeholder)
                        .i32_const(step)
                        .binop(BinaryOp::I32Add);
                },
            );
            let id = builder.finish(vec![n], &mut module.funcs);
            // Each copy calls itself, not the other: a cycle of its own.
            let itself = HashMap::from([(placeholder, id)]);
            if let FunctionKind::Local(local) = &mut module.funcs.get_mut(id).kind {
                let entry = local.entry_block();
                walrus::ir::dfs_pre_order_mut(&mut Redirect(&itself), local, entry);
            }
            module.exports.add(name, id);
        }
        walrus::passes::gc::run(&mut module);
        module.emit_wasm()
    }

    fn run(bytes: &[u8], name: &str, n: i32) -> Result<i32> {
        use wasmi::{Engine, Linker, Module, Store};
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let instance = Linker::<()>::new(&engine)
            .instantiate_and_start(&mut store, &Module::new(&engine, bytes)?)?;
        Ok(instance
            .get_typed_func::<i32, i32>(&store, name)?
            .call(&mut store, n)?)
    }

    fn functions(bytes: &[u8]) -> Result<usize> {
        Ok(walrus::Module::from_buffer(bytes)?.funcs.iter().count())
    }

    #[test]
    fn copies_that_call_themselves_merge_and_a_different_body_does_not() -> Result<()> {
        let module = counters();
        assert_eq!(representatives(&module)?, vec![0, 0, 2]);
        let (merged, count) = merge_duplicates(&module)?;
        assert_eq!(count, 1);
        assert_eq!(functions(&module)?, 3);
        assert_eq!(functions(&merged)?, 2, "the second copy is gone");
        for n in [0, 1, 5, 40] {
            for name in ["a", "b", "c"] {
                assert_eq!(
                    run(&merged, name, n)?,
                    run(&module, name, n)?,
                    "{name}({n})"
                );
            }
        }
        Ok(())
    }
}
