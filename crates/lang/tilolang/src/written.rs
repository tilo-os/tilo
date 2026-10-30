//! Written views: a `view` a person or an agent writes, kept as that person's own code
//! (`docs/designs/app-language/agent-ux.md`, "Answers as views").
//!
//! A written view is a real `view` declaration, as an app declares one, with the functions it
//! calls: `view open_threads(m: mail.Mail) { ... }`. It names no app with `use`: every package
//! the workspace has installed is in scope by the last part of its id (`mail`, `tasks`), resolved
//! when the view is checked, so a view reads whatever types the workspace has, and one over an
//! app the workspace lacks says so. It is compiled into a package of its own,
//! `agent.written.<hash>`, whose one view is the written one; the runtime draws it with its author
//! as its code author.
//!
//! A bare view body (`card(tasks.tasks) { ... }`), which answers were before they were
//! declarations, reads as the body of a view that takes nothing.

use crate::diag::{Diagnostic, Span};
use crate::hash::sha256_hex;
use crate::ids::{IdsLock, SeededEntropy};
use crate::meta::{PackageMeta, ParamMeta};
use crate::resolve::default_alias;
use crate::source::PackageSource;

/// The app prefix of a written view's package: never visible to other code.
pub const WRITTEN_VIEW_APP_PREFIX: &str = "agent.written.";

/// The path the definition has in its package, which nothing shows: diagnostics are located in
/// the definition itself.
const DEFINITION: &str = "view.tl";
/// The path of the package's header and its imports.
const HEADER: &str = "package.tl";

/// What a bare view body is wrapped in, on its first line, so its lines stay where they were.
const BARE_OPEN: &str = "view shown() { ";

/// A compiled written view.
#[derive(Clone, Debug)]
pub struct CompiledView {
    pub wasm: Vec<u8>,
    pub meta: PackageMeta,
    /// The view's id in [`Self::meta`]: its export is `v_<id>`.
    pub view: String,
    /// Its name, as written.
    pub name: String,
    /// What it takes, each type as metadata writes it (`so.tilo.lang.mail.Thread`).
    pub params: Vec<ParamMeta>,
}

impl CompiledView {
    /// The type of resource it shows, when it takes exactly one resource: what a person can make
    /// it the default view of. `None` for a view that stands alone or takes several.
    #[must_use]
    pub fn shows(&self) -> Option<&str> {
        match self.params.as_slice() {
            [one] => Some(one.ty.as_str()),
            _ => None,
        }
    }
}

/// Whether `source` is a declaration (`view …`, `fn …`, led by comments) rather than a bare
/// view body. A view's body never starts with a word that starts a declaration.
#[must_use]
pub fn is_declaration(source: &str) -> bool {
    let first = source
        .lines()
        .map(str::trim_start)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
        .unwrap_or_default();
    // Every word a declaration starts with: a written view holds only `view` and `fn`, and the
    // others are refused as such rather than read as a view's body.
    [
        "view ",
        "fn ",
        "use ",
        "type ",
        "enum ",
        "root ",
        "action ",
        "flow ",
        "default view ",
        "row view ",
        "scenario ",
        "primary ",
        "destructive ",
        "migration ",
    ]
    .iter()
    .any(|lead| first.starts_with(lead))
}

/// Compile the written view `source` by `author` against the packages `visible` (every
/// installed app; the host's own package and other written code are left out). Its writer is
/// part of its package's identity: the same text two people wrote is two packages, each
/// bounded by its own writer.
///
/// # Errors
/// The diagnostics, located in `source`: a definition that is not one `view` (with functions),
/// a name no installed app has, or anything a view in an app may not do.
pub fn compile_view_definition(
    source: &str,
    author: &str,
    visible: &[PackageMeta],
) -> Result<CompiledView, Vec<Diagnostic>> {
    let bare = !is_declaration(source);
    let definition = if bare {
        format!("{BARE_OPEN}{source}\n}}")
    } else {
        source.to_owned()
    };
    let mut fingerprint = format!("{author}\0{definition}");
    let apps: Vec<&PackageMeta> = visible
        .iter()
        .filter(|m| {
            m.app != crate::host::HOST_APP
                && !m.app.starts_with("agent.")
                && !m.app.starts_with(WRITTEN_VIEW_APP_PREFIX)
        })
        .collect();
    for m in &apps {
        fingerprint.push_str(&format!("\n{}:{}", m.app, m.module_hash));
    }
    let key = sha256_hex(fingerprint.as_bytes());
    let short = key.get(..16).unwrap_or(&key);
    // A segment of an app id starts with a letter.
    let app = format!("{WRITTEN_VIEW_APP_PREFIX}v{short}");
    // Every app by the last part of its id; when two share it, the first keeps the name and
    // the other is not in scope, as no `use` can name it twice.
    let mut header = format!("app {app} \"View\"\n");
    let mut aliases: Vec<String> = Vec::new();
    for m in &apps {
        let alias = default_alias(&m.app);
        if aliases.contains(&alias) {
            continue;
        }
        header.push_str(&format!("use {} as {alias}\n", m.app));
        aliases.push(alias);
    }
    let package = PackageSource::new([
        (HEADER.to_owned(), header),
        (DEFINITION.to_owned(), definition.clone()),
    ]);
    let unbare = |diags: Vec<Diagnostic>| relocate(diags, bare);
    // A `use` line, which only a package's first file may hold: every app is in scope already.
    let mut offset = 0;
    let mut uses = Vec::new();
    for line in definition.split_inclusive('\n') {
        if line.starts_with("use ") {
            let mut d = Diagnostic::new(
                "E116",
                Span::new(offset, offset + line.trim_end().len()),
                "a written view names no app with `use`: every installed app is in scope by its \
                 name already (`mail`, `tasks`)",
            );
            d.locate(&definition);
            uses.push(d);
        }
        offset += line.len();
    }
    if !uses.is_empty() {
        return Err(unbare(uses));
    }
    // Only views and the functions they call: a written view declares no types, actions or
    // roots, and names no app with `use`.
    let files = crate::package::parse_package_files(&package).map_err(unbare)?;
    let items: Vec<&crate::ast::Item> = files
        .iter()
        .filter(|f| f.path == DEFINITION)
        .flat_map(|f| f.items.iter())
        .collect();
    let mut diags = Vec::new();
    let mut views = Vec::new();
    for item in &items {
        match item {
            crate::ast::Item::View(v) => views.push(v),
            crate::ast::Item::Fn(_) => {}
            other => diags.push(Diagnostic::new(
                "E116",
                item_span(other),
                "a written view holds one `view` and the functions it calls; every installed \
                 app is already in scope by its name (`mail`, `tasks`), and types and actions \
                 belong to apps",
            )),
        }
    }
    if views.len() != 1 {
        diags.push(Diagnostic::new(
            "E117",
            views.get(1).map_or(Span::new(0, 0), |v| v.name.span),
            format!(
                "a written view is one `view`, such as `view open_threads(m: mail.Mail) {{ ... }}`; \
                 this has {}",
                views.len()
            ),
        ));
    }
    if !diags.is_empty() {
        // Spans from the parse are in the package's shared span space: the header comes first.
        let base = package
            .files
            .iter()
            .find(|f| f.path == HEADER)
            .map_or(0, |f| f.text.len() + 1);
        for d in &mut diags {
            d.span = Span::new(
                d.span.start.saturating_sub(base),
                d.span.end.saturating_sub(base),
            );
            d.file = Some(DEFINITION.to_owned());
            d.locate(&definition);
        }
        return Err(unbare(diags));
    }
    let name = views[0].name.name.clone();
    let compiled = crate::package::compile_package_source_with(
        &package,
        &IdsLock::default(),
        visible,
        &mut SeededEntropy(u64::from_str_radix(short, 16).unwrap_or(0)),
    )
    .map_err(unbare)?;
    let meta_view = compiled
        .meta
        .views
        .iter()
        .find(|v| v.name == name && v.row_of.is_none())
        .ok_or_else(|| {
            vec![Diagnostic::new(
                "E117",
                Span::new(0, 0),
                format!("the view `{name}` was not compiled"),
            )]
        })?;
    Ok(CompiledView {
        view: meta_view.id.clone(),
        name,
        params: meta_view.params.clone(),
        wasm: compiled.wasm,
        meta: compiled.meta,
    })
}

fn item_span(item: &crate::ast::Item) -> Span {
    use crate::ast::Item;
    match item {
        Item::Use(u) => u.span,
        Item::Type(t) => t.span,
        Item::Alias(a) => a.span,
        Item::Enum(e) => e.span,
        Item::Root(r) => r.span,
        Item::DefaultView(d) | Item::RowView(d) => d.span,
        Item::Scenario(s) => s.span,
        Item::Action(a) => a.span,
        Item::Flow(f) => f.span,
        Item::View(v) => v.span,
        Item::Fn(f) => f.span,
    }
}

/// Diagnostics as the person reads them: in the definition they wrote (no file), and for a
/// bare body without the wrapping its first line got.
fn relocate(mut diags: Vec<Diagnostic>, bare: bool) -> Vec<Diagnostic> {
    let shift = if bare { BARE_OPEN.len() } else { 0 };
    for d in &mut diags {
        if d.file.as_deref() != Some(DEFINITION) {
            continue;
        }
        d.file = None;
        if shift > 0 {
            d.span = Span::new(
                d.span.start.saturating_sub(shift),
                d.span.end.saturating_sub(shift),
            );
            if d.line == 1 {
                d.col = d.col.saturating_sub(shift).max(1);
            }
        }
    }
    diags
}
