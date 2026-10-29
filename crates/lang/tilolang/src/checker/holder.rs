//! Types that name a container (`type Notes = Collection<Note>`): a resource whose one field is
//! that container. Code uses such a resource as the container it holds: `notes.insert(n)`,
//! `for n in notes`, `notes.filter(...)`, and passing it where a list goes.

use super::{Checker, Ex};
use crate::ast;
use crate::diag::Span;
use crate::resolve::FieldInfo;
use crate::types::{Ty, TypeKey};

impl Checker<'_> {
    /// The container a value of type `ty` holds, when `ty` names one.
    pub(super) fn held_of(&self, ty: &Ty) -> Option<(TypeKey, FieldInfo)> {
        let Ty::Record(key) = ty else { return None };
        let f = self.env.type_info(key)?.held()?.clone();
        Some((key.clone(), f))
    }

    /// The container `ex` holds, read as a list, when its type names a collection or a set;
    /// otherwise `ex` itself.
    pub(super) fn unhold(&mut self, ex: Ex, span: Span) -> Ex {
        match self.held_of(&ex.ty) {
            Some((_, f)) if matches!(f.ty, Ty::Collection(_) | Ty::Set(_)) => {
                self.field_read(ex, &ast::Ident { name: f.name, span })
            }
            _ => ex,
        }
    }
}
