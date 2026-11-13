//! The compiler's reading of what a package declares runs by itself: the action a trigger invokes, the
//! records it runs for, and when. A trigger names one of the package's own actions and hands it the
//! record or a root.

use super::{CKind, Resolver, TriggerArg, TriggerInfo, TriggerWhen, id_of};
use crate::ast::{self, Item};
use crate::ids::IdsLock;
use crate::types::{Ty, TypeKey};

fn record_key(ty: &Ty) -> Option<TypeKey> {
    match ty {
        Ty::Record(key) => Some(key.clone()),
        _ => None,
    }
}

/// Whether a parameter of type `expected` takes a resource of type `actual` (a parameter may hold it or
/// not: an `Option<T>` takes a `T`).
fn accepts(expected: &Ty, actual: &Ty) -> bool {
    match expected {
        Ty::Option(inner) => **inner == *actual,
        other => other == actual,
    }
}

fn is_time(ty: &Ty) -> bool {
    matches!(ty, Ty::Time) || matches!(ty, Ty::Option(inner) if **inner == Ty::Time)
}

impl Resolver<'_> {
    /// The triggers of the module: the action each runs, the type whose resources it runs for, and when. A
    /// trigger runs where the package's actions run, so it names one of the package's own actions and hands
    /// it the record or a root.
    pub(super) fn triggers(&mut self, module: &ast::Module, lock: &IdsLock) {
        for item in &module.items {
            let Item::Trigger(t) = item else { continue };
            let Some(param) = &t.param else {
                self.err(
                    "E130",
                    t.name.span,
                    "a trigger runs for the records of one type: write `trigger remind(t: Task) at t.at: notify_due(t)`",
                );
                continue;
            };
            if param.default.is_some() || param.place.is_some() || param.label.is_some() {
                self.err(
                    "E130",
                    t.name.span,
                    "a trigger's parameter is the record it runs for, and no call gives it: it takes no default, place or label",
                );
            }
            let ty = self.resolve_type(&param.ty, false);
            let Some(key) = record_key(&ty) else {
                self.err(
                    "E131",
                    param.ty.span,
                    "a trigger runs for the records of a type of this package (`t: Task`); its parameter is one",
                );
                continue;
            };
            let Some(info) = self
                .env
                .type_info(&key)
                .filter(|info| info.key.app == self.env.app)
                .cloned()
            else {
                self.err(
                    "E131",
                    param.ty.span,
                    format!("a trigger runs for this package's records, and `{}` is not one of its types", key.name),
                );
                continue;
            };
            let when = match &t.when {
                ast::TriggerWhen::Every { amount, unit, span } => {
                    match crate::duration::seconds(*amount, unit) {
                        Some(secs) if secs > 0 => TriggerWhen::Every(secs),
                        _ => {
                            self.err(
                                "E132",
                                *span,
                                "`every` takes a length of time above zero: `every 30m`, `every 6h`, `every 1d`",
                            );
                            continue;
                        }
                    }
                }
                ast::TriggerWhen::At { expr, span } => {
                    let named = match &expr.kind {
                        ast::ExprKind::Field { base, name } if matches!(&base.kind, ast::ExprKind::Ident(p) if *p == param.name.name) => {
                            Some(name.clone())
                        }
                        _ => None,
                    };
                    let Some(named) = named else {
                        self.err(
                            "E133",
                            *span,
                            format!(
                                "`at` reads the time `{}` holds: write `at {}.<the field>`",
                                param.name.name, param.name.name
                            ),
                        );
                        continue;
                    };
                    let Some(field) = info.field(&named.name) else {
                        self.err(
                            "E133",
                            named.span,
                            format!("`{}` has no field `{}`: `at` reads the instant one of its fields holds", info.key.name, named.name),
                        );
                        continue;
                    };
                    if !is_time(&field.ty) {
                        self.err(
                            "E133",
                            named.span,
                            format!(
                                "`{}.{}` is not a time: `at` reads a `Time` or an `Option<Time>` field, and the host reads it without running the package",
                                info.key.name, named.name
                            ),
                        );
                        continue;
                    }
                    TriggerWhen::At(field.id.clone())
                }
            };
            let ast::ExprKind::Call { callee, args, .. } = &t.call.kind else {
                self.err(
                    "E134",
                    t.call.span,
                    "a trigger ends with the action it runs: `trigger remind(t: Task) at t.at: notify_due(t)`",
                );
                continue;
            };
            let ast::ExprKind::Ident(named) = &callee.kind else {
                self.err(
                    "E134",
                    callee.span,
                    "a trigger runs one of this package's actions, by its name",
                );
                continue;
            };
            let Some(action) = self
                .env
                .own_callable(named)
                .filter(|action| action.kind == CKind::Action)
                .cloned()
            else {
                self.err(
                    "E134",
                    t.call.span,
                    format!("this package declares no action `{named}`: a trigger runs it as the app's own invocation"),
                );
                continue;
            };
            let mut built = Vec::new();
            let mut sound = args.len() <= action.params.len();
            if !sound {
                self.err(
                    "E135",
                    t.call.span,
                    format!(
                        "`{}` takes {} argument(s), and the trigger gives it {}",
                        action.name,
                        action.params.len(),
                        args.len()
                    ),
                );
            }
            for (arg, expected) in args.iter().zip(&action.params) {
                match &arg.value.kind {
                    ast::ExprKind::Ident(p) if *p == param.name.name => {
                        if !accepts(&expected.ty, &ty) {
                            self.err(
                                "E135",
                                arg.value.span,
                                format!(
                                    "the record the trigger runs for is a `{}`, and `{}` takes another type there",
                                    info.key.name, expected.name
                                ),
                            );
                            sound = false;
                            continue;
                        }
                        built.push(TriggerArg::Subject);
                    }
                    ast::ExprKind::Root(root) => match self.own_root(root) {
                        Some((key, id)) => {
                            if !accepts(&expected.ty, &Ty::Record(key)) {
                                self.err(
                                    "E135",
                                    arg.value.span,
                                    format!(
                                        "`{}` takes no such root: a root is a record of its own type, and the parameter is `{}`",
                                        expected.name,
                                        expected.ty.meta_text(&self.env.app)
                                    ),
                                );
                                sound = false;
                                continue;
                            }
                            built.push(TriggerArg::Root(id));
                        }
                        None => sound = false,
                    },
                    _ => {
                        self.err(
                            "E135",
                            arg.value.span,
                            "a trigger passes the record it runs for, or a root (`root T`): the platform makes no other argument without running the package",
                        );
                        sound = false;
                    }
                }
            }
            for missing in action.params.iter().skip(args.len()) {
                if missing.default.is_none() {
                    self.err(
                        "E135",
                        t.call.span,
                        format!(
                            "`{}` takes `{}`, which the trigger gives it no argument and the action has no default for",
                            action.name, missing.name
                        ),
                    );
                    sound = false;
                }
            }
            if !sound {
                continue;
            }
            self.env.triggers.push(TriggerInfo {
                name: t.name.name.clone(),
                id: id_of(lock, &crate::ids::trigger_key(&t.name.name)),
                doc: t.doc.join("\n"),
                action: action.id.clone(),
                ty: info.id.clone(),
                args: built,
                when,
            });
        }
    }

    /// The root `root T` names: its type and its id, for one of this package's roots. A diagnostic says why
    /// not for anything else.
    fn own_root(&mut self, te: &ast::TypeExpr) -> Option<(crate::types::TypeKey, String)> {
        let named = match &te.kind {
            ast::TypeExprKind::Named(path) => match path.as_slice() {
                [name] => Some(name.clone()),
                _ => None,
            },
            _ => None,
        };
        let found = named.and_then(|name| {
            self.env
                .own_type(&name.name)
                .filter(|info| self.env.roots.contains(&info.key))
                .map(|info| (info.key.clone(), info.id.clone()))
        });
        if found.is_none() {
            self.err(
                "E136",
                te.span,
                "`root` takes a root type of this package (`root T`): a trigger passes the record it runs for, or one of its roots",
            );
        }
        found
    }
}
