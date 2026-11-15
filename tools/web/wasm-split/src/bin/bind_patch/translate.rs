//! What a body becomes when the bind writes it again: the translation of a
//! linked body into the bound module's index space and locals, the
//! renumbering of a body whose indices moved, and whether it moved at all
//! (`src/bin/bind_patch.rs`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use anyhow::{Context, Result};
use wasm_encoder::reencode::{Error as ReError, Reencode, utils};
use wasm_encoder::{Encode, Instruction};
use wasmparser_content as wp;
use wp::{Operator, ValType};

use super::{Meta, RefsOfBody, needs_bind, read_leb, write_leb};

/// Whether every index in `body`'s body already is its new one: the bytes a
/// full bind writes, kept where they are.
pub(super) fn unchanged_indices(
    body: &RefsOfBody,
    funcs: &[u32],
    types: &dyn Fn(u32) -> Result<u32>,
) -> bool {
    body.funcs
        .iter()
        .all(|function| funcs.get(*function as usize).copied() == Some(*function))
        && body
            .types
            .iter()
            .all(|ty| types(*ty).is_ok_and(|new| new == *ty))
}

/// A kept body with its function and type indices renumbered.
pub(super) fn renumber_body(
    body: &[u8],
    funcs: &[u32],
    types: &dyn Fn(u32) -> u32,
) -> Result<Vec<u8>> {
    let function = wp::FunctionBody::new(wp::BinaryReader::new(body, 0));
    let mut operators = function.get_operators_reader()?;
    let header = operators.original_position();
    let mut out = Vec::with_capacity(body.len());
    out.extend_from_slice(&body[..header]);
    let function_index = |index: u32| -> Result<u32> {
        match funcs.get(index as usize) {
            Some(target) if *target != u32::MAX => Ok(*target),
            _ => Err(needs_bind(format!("function {index} is unresolved"))),
        }
    };
    let type_index = |ty: u32| -> Result<u32> {
        let target = types(ty);
        if target == u32::MAX {
            return Err(needs_bind(format!("type {ty} is unresolved")));
        }
        Ok(target)
    };
    while !operators.eof() {
        let start = operators.original_position();
        let operator = operators.read()?;
        let end = operators.original_position();
        match &operator {
            // One opcode byte, then the function's index.
            Operator::Call {
                function_index: index,
            }
            | Operator::ReturnCall {
                function_index: index,
            }
            | Operator::RefFunc {
                function_index: index,
            } => {
                out.push(body[start]);
                write_leb(&mut out, function_index(*index)?);
            }
            // One opcode byte, then the type's index, then the rest.
            Operator::CallIndirect { type_index: ty, .. }
            | Operator::ReturnCallIndirect { type_index: ty, .. } => {
                out.push(body[start]);
                let mut at = start + 1;
                let _old = read_leb(body, &mut at)?;
                write_leb(&mut out, type_index(*ty)?);
                out.extend_from_slice(&body[at..end]);
            }
            Operator::Block { blockty }
            | Operator::Loop { blockty }
            | Operator::If { blockty }
            | Operator::Try { blockty }
                if matches!(blockty, wp::BlockType::FuncType(_)) =>
            {
                struct Map<'a> {
                    funcs: &'a [u32],
                    types: &'a dyn Fn(u32) -> u32,
                }
                impl Reencode for Map<'_> {
                    type Error = String;
                    fn function_index(&mut self, function: u32) -> Result<u32, ReError<String>> {
                        match self.funcs.get(function as usize) {
                            Some(target) if *target != u32::MAX => Ok(*target),
                            _ => Err(ReError::UserError(format!(
                                "function {function} is unresolved"
                            ))),
                        }
                    }
                    fn type_index(&mut self, ty: u32) -> Result<u32, ReError<String>> {
                        let target = (self.types)(ty);
                        if target == u32::MAX {
                            return Err(ReError::UserError(format!("type {ty} is unresolved")));
                        }
                        Ok(target)
                    }
                }
                let mut map = Map { funcs, types };
                let instruction = utils::instruction(&mut map, operator)
                    .map_err(|error| needs_bind(format!("renumbering a body: {error:?}")))?;
                instruction.encode(&mut out);
            }
            _ => out.extend_from_slice(&body[start..end]),
        }
    }
    Ok(out)
}

/// A body translated from the linked module's index space to the bound one.
///
/// walrus emits a body's locals as its arguments, then the used non-argument
/// locals grouped by type in `ValType` order; its instructions canonically,
/// with function and type indices mapped and local indices renumbered.
pub(super) fn translate(
    linked: &[u8],
    span: &Range<usize>,
    lmeta: &Meta,
    types: &[u32],
    map: &[u32],
    local: usize,
) -> Result<Vec<u8>> {
    let bytes = &linked[span.clone()];
    let body = wp::FunctionBody::new(wp::BinaryReader::new(bytes, span.start));
    let nargs = lmeta.nargs(local);
    let mut decl = Vec::new();
    for pair in body.get_locals_reader()? {
        let (count, ty) = pair?;
        decl.push((count, ty));
    }
    let mut operators = body.get_operators_reader()?;
    let mut used = HashSet::new();
    while !operators.eof() {
        match operators.read()? {
            Operator::LocalGet { local_index }
            | Operator::LocalSet { local_index }
            | Operator::LocalTee { local_index } => {
                used.insert(local_index);
            }
            _ => {}
        }
    }
    // The used locals, grouped by type: each type's locals in declaration
    // order, and the types in `ValType` order.
    let mut groups: BTreeMap<ValType, Vec<u32>> = BTreeMap::new();
    let mut index = nargs;
    for (count, ty) in &decl {
        for _ in 0..*count {
            if used.contains(&index) {
                groups.entry(*ty).or_default().push(index);
            }
            index += 1;
        }
    }
    let mut locals = Vec::new();
    let mut local_index: HashMap<u32, u32> = (0..nargs).map(|i| (i, i)).collect();
    let mut next = nargs;
    for (ty, indices) in &groups {
        locals.push((indices.len() as u32, wasm_encoder::ValType::try_from(*ty)?));
        for source in indices {
            local_index.insert(*source, next);
            next += 1;
        }
    }
    struct Tr<'a> {
        types: &'a [u32],
        map: &'a [u32],
        locals: &'a HashMap<u32, u32>,
    }
    impl Reencode for Tr<'_> {
        type Error = String;
        fn function_index(&mut self, function: u32) -> Result<u32, ReError<String>> {
            match self.map.get(function as usize) {
                Some(target) if *target != u32::MAX => Ok(*target),
                _ => Err(ReError::UserError(format!(
                    "function {function} is unresolved"
                ))),
            }
        }
        fn type_index(&mut self, ty: u32) -> Result<u32, ReError<String>> {
            match self.types.get(ty as usize) {
                Some(target) if *target != u32::MAX => Ok(*target),
                _ => Err(ReError::UserError(format!("type {ty} is unresolved"))),
            }
        }
    }
    let mut tr = Tr {
        types,
        map,
        locals: &local_index,
    };
    let mut reader = body.get_operators_reader()?;
    let mut function = wasm_encoder::Function::new(locals);
    while !reader.eof() {
        let instruction = utils::parse_instruction(&mut tr, &mut reader)
            .map_err(|error| needs_bind(format!("translating a body: {error:?}")))?;
        let instruction = match instruction {
            Instruction::LocalGet(i) => {
                Instruction::LocalGet(*tr.locals.get(&i).context("a local is unresolved")?)
            }
            Instruction::LocalSet(i) => {
                Instruction::LocalSet(*tr.locals.get(&i).context("a local is unresolved")?)
            }
            Instruction::LocalTee(i) => {
                Instruction::LocalTee(*tr.locals.get(&i).context("a local is unresolved")?)
            }
            other => other,
        };
        function.instruction(&instruction);
    }
    // `Function::encode` writes the body's size LEB first; the patch writes
    // its own, so the body is kept as its content alone.
    let mut out = Vec::new();
    function.encode(&mut out);
    let mut at = 0;
    let _size = read_leb(&out, &mut at)?;
    Ok(out.split_off(at))
}

pub(super) fn bound_body_instr_start(body: &[u8]) -> Result<usize> {
    Ok(wp::FunctionBody::new(wp::BinaryReader::new(body, 0))
        .get_operators_reader()?
        .original_position())
}
