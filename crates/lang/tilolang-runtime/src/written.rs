//! Written views (`tilolang::written`): a `view` a person or an agent wrote, kept as their own
//! code, compiled against every package this runtime holds and drawn with its writer as its code
//! author, so whatever it does with an action asks first unless the viewer trusts that writer.
//!
//! The view reads as the viewer reads: it runs in the viewer's runtime, over the records the
//! viewer holds, as every view does. Its writer only bounds what it may do.

use std::rc::Rc;

use tilolang::{CompiledView, Diagnostic, PackageMeta, Span};

use crate::core::Core;
use crate::error::RuntimeError;
use crate::ids::{ModuleHash, Principal};
use crate::meta::ParamMeta;
use crate::registry::Package;
use crate::render::ViewTarget;
use crate::value::Value;
use crate::view_instance::ViewInstance;

/// A written view compiled and loaded.
pub struct WrittenView {
    pub(crate) view: CompiledView,
    pub(crate) pkg: Rc<Package>,
}

impl WrittenView {
    /// Where it renders.
    pub(crate) fn target(&self) -> ViewTarget {
        ViewTarget {
            module: self.pkg.hash.clone(),
            export: format!("v_{}", self.view.view),
            params: self.view.params.clone(),
        }
    }
}

pub(crate) type CachedView = Result<Rc<WrittenView>, Rc<Vec<Diagnostic>>>;

/// What a written view is, once it checked: its name, what it takes, and the type it shows
/// when it takes one resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrittenViewInfo {
    pub name: String,
    pub params: Vec<ParamMeta>,
    /// The type of the one resource it takes (`so.tilo.lang.mail.Thread`), which it can be the
    /// default view of; `None` for a view that stands alone.
    pub shows: Option<String>,
}

/// A diagnostic as one line: `line:col: code message`.
pub(crate) fn diagnostic_line(d: &Diagnostic) -> String {
    format!("{}:{}: {} {}", d.line, d.col, d.code, d.message)
}

impl Core {
    /// The packages code written here sees, the newest of each app, without blocks, programs
    /// and other written views; and the fingerprint of that set.
    pub(crate) fn visible_metas(&self) -> (Vec<PackageMeta>, blake3::Hasher) {
        let pkgs: Vec<Rc<Package>> = self
            .registry
            .borrow()
            .packages()
            .into_iter()
            .filter(|p| !is_written_code(&p.meta.app))
            .collect();
        let mut fingerprint = blake3::Hasher::new();
        for p in &pkgs {
            fingerprint.update(p.hash.0.as_bytes());
        }
        let mut visible: Vec<PackageMeta> = Vec::new();
        for p in pkgs.iter().rev() {
            if !visible.iter().any(|m| m.app == p.meta.app) {
                visible.push(p.meta.clone());
            }
        }
        visible.reverse();
        (visible, fingerprint)
    }

    /// The writer of the written view whose module is `module`, as its code author.
    pub(crate) fn written_author(&self, module: &ModuleHash) -> Option<ModuleHash> {
        self.embeds.borrow().authors.get(module).cloned()
    }

    /// The written view `source` by `author`, compiled against the packages loaded now, cached by
    /// its source, its writer and that package set. Its writer is part of what it is: the same
    /// text two people wrote is two modules, each bounded by its own writer.
    pub(crate) fn written_view(&self, source: &str, author: &Principal) -> CachedView {
        let (visible, mut fingerprint) = self.visible_metas();
        fingerprint.update(b"\0view\0");
        fingerprint.update(author.0.as_bytes());
        fingerprint.update(b"\0");
        fingerprint.update(source.as_bytes());
        let key = fingerprint.finalize().to_hex().to_string();
        if let Some(hit) = self.embeds.borrow().views.get(&key) {
            return hit.clone();
        }
        let built = match tilolang::compile_view_definition(source, &author.0, &visible) {
            Ok(view) => match self.load_block(&view.wasm, &view.meta) {
                Ok(pkg) => {
                    self.embeds
                        .borrow_mut()
                        .authors
                        .insert(pkg.hash.clone(), ModuleHash::principal(&author.0));
                    Ok(Rc::new(WrittenView { view, pkg }))
                }
                Err(e) => Err(Rc::new(vec![Diagnostic::new(
                    "E000",
                    Span::default(),
                    e.to_string(),
                )])),
            },
            Err(diags) => Err(Rc::new(diags)),
        };
        self.embeds.borrow_mut().views.insert(key, built.clone());
        built
    }

    /// What the written view `source` by `author` is.
    pub(crate) fn check_written_view(
        &self,
        source: &str,
        author: &Principal,
    ) -> Result<WrittenViewInfo, Vec<Diagnostic>> {
        let view = self
            .written_view(source, author)
            .map_err(|d| (*d).clone())?;
        Ok(WrittenViewInfo {
            name: view.view.name.clone(),
            params: view.view.params.clone(),
            shows: view.view.shows().map(str::to_owned),
        })
    }

    /// Open the written view `source` for `viewer` over `args`, its code author `author`.
    pub(crate) fn open_written_view(
        self: &Rc<Self>,
        viewer: &Principal,
        source: &str,
        author: &Principal,
        args: Vec<Value>,
    ) -> Result<ViewInstance, RuntimeError> {
        let view = self.written_view(source, author).map_err(|diags| {
            RuntimeError::Package(format!(
                "the view does not compile:\n{}",
                diags
                    .iter()
                    .map(diagnostic_line)
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        })?;
        if args.len() != view.view.params.len() {
            return Err(RuntimeError::Package(format!(
                "the view `{}` takes {} arguments, not {}",
                view.view.name,
                view.view.params.len(),
                args.len()
            )));
        }
        let target = view.target();
        let window = crate::view_instance::default_window(&target, &args);
        ViewInstance::open(Rc::clone(self), (viewer.clone(), window), target, args)
    }
}

/// Whether `app` is code an agent or a person wrote rather than an installed package: a block,
/// a program, or a written view. Other written code never sees it.
pub(crate) fn is_written_code(app: &str) -> bool {
    app.starts_with("agent.") || app.starts_with(tilolang::WRITTEN_VIEW_APP_PREFIX)
}
