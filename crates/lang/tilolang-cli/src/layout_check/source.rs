//! Where a node of a window comes from: the key the compiler gave it names its view and the
//! place in that view's source (`checker::ui::node_key`: `<view id>.<template calls><node>`,
//! then the identity of the items it is in), which this reads back into a view path and a
//! source span.

use std::collections::HashMap;
use std::path::PathBuf;

use tilolang::ast::{self, UiKind, UiStmt};
use tilolang::source::PackageSource;

/// A place in a package's source.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Span {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// A view of a package.
struct View {
    /// The package's app id.
    app: String,
    name: String,
    span: Span,
}

/// A UI call of a view.
struct Call {
    /// What it calls, as written (`ui.ListRow`, `row`, `std.list`).
    what: String,
    span: Span,
}

/// One package's views and UI calls, by id and node.
struct Package {
    calls: HashMap<ast::NodeId, Call>,
}

/// What a key of a window's tree resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// The app whose source it is.
    pub app: String,
    pub view: String,
    /// The UI call in that view's source, when the key names one: the outermost call of the
    /// app's own source (a template's inner nodes are shown at the call of the template).
    pub node: Option<String>,
    pub span: Span,
}

/// Every view and UI call of an app and the packages it uses.
pub struct SourceMap {
    views: HashMap<String, View>,
    packages: HashMap<String, Package>,
}

/// A source's files laid out as the compiler lays them out (`package::placed`): in path order,
/// each starting one past the previous one's end.
struct Placed {
    files: Vec<(String, usize, String)>,
}

impl Placed {
    fn new(source: &PackageSource, dir: &std::path::Path) -> Self {
        let mut files: Vec<_> = source.files.iter().collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut base: usize = 0;
        let files = files
            .into_iter()
            .map(|file| {
                let at = base;
                base = base.saturating_add(file.text.len()).saturating_add(1);
                let shown = PathBuf::from(&file.path);
                let shown = shown
                    .strip_prefix(dir.parent().unwrap_or(dir))
                    .map_or_else(|_| file.path.clone(), |p| p.display().to_string());
                (shown, at, file.text.clone())
            })
            .collect();
        Self { files }
    }

    fn span(&self, offset: usize) -> Span {
        let (file, base, text) = self
            .files
            .iter()
            .rev()
            .find(|(_, base, _)| *base <= offset)
            .or_else(|| self.files.first())
            .map_or(("?", 0, ""), |(file, base, text)| {
                (file.as_str(), *base, text.as_str())
            });
        let local = offset.saturating_sub(base).min(text.len());
        let before = text.get(..local).unwrap_or_default();
        let line = before.matches('\n').count().saturating_add(1);
        let column = before
            .rsplit('\n')
            .next()
            .map_or(0, |l| l.chars().count())
            .saturating_add(1);
        Span {
            file: file.to_owned(),
            line,
            column,
        }
    }

    fn text(&self, span: tilolang::Span) -> &str {
        self.files
            .iter()
            .rev()
            .find(|(_, base, _)| *base <= span.start)
            .and_then(|(_, base, text)| {
                text.get(span.start.saturating_sub(*base)..span.end.saturating_sub(*base))
            })
            .unwrap_or_default()
    }
}

/// The callee of a UI call as written, without its arguments.
fn callee(text: &str) -> String {
    let end = text.find(['(', '{', '\n']).unwrap_or(text.len());
    text.get(..end).unwrap_or(text).trim().to_owned()
}

fn collect(stmts: &[UiStmt], placed: &Placed, calls: &mut HashMap<ast::NodeId, Call>) {
    for stmt in stmts {
        match &stmt.kind {
            UiKind::Call(call) => {
                calls.insert(
                    call.node,
                    Call {
                        what: callee(placed.text(call.call.span)),
                        span: placed.span(stmt.span.start),
                    },
                );
                if let Some(block) = &call.block {
                    collect(block, placed, calls);
                }
            }
            UiKind::For { body, .. } => collect(body, placed, calls),
            UiKind::If { then, els, .. } | UiKind::IfLet { then, els, .. } => {
                collect(then, placed, calls);
                if let Some(els) = els {
                    collect(els, placed, calls);
                }
            }
            UiKind::Match { arms, .. } => {
                for arm in arms {
                    collect(&arm.body, placed, calls);
                }
            }
            UiKind::Slot { node, name, body } => {
                calls.insert(
                    *node,
                    Call {
                        what: format!("{}:", name.name),
                        span: placed.span(stmt.span.start),
                    },
                );
                collect(body, placed, calls);
            }
            UiKind::Fact { node, .. } => {
                calls.insert(
                    *node,
                    Call {
                        what: "fact".to_owned(),
                        span: placed.span(stmt.span.start),
                    },
                );
            }
            UiKind::Let { .. } | UiKind::State(_) | UiKind::On { .. } => {}
        }
    }
}

impl SourceMap {
    /// The views of the packages built from `dirs` (each with its compiled metadata).
    pub fn new(packages: &[tilolang::source::BuiltPackage]) -> Self {
        let mut views = HashMap::new();
        let mut maps = HashMap::new();
        for built in packages {
            let app = built.compiled.meta.app.clone();
            let placed = Placed::new(&built.source, &built.dir);
            let Ok(module) = tilolang::parse_package(&built.source) else {
                continue;
            };
            let mut calls = HashMap::new();
            let mut spans = HashMap::new();
            for item in &module.items {
                if let ast::Item::View(view) = item {
                    spans.insert(view.name.name.clone(), placed.span(view.span.start));
                    collect(&view.body, &placed, &mut calls);
                }
            }
            for view in &built.compiled.meta.views {
                let span = spans
                    .get(&view.name)
                    .cloned()
                    .unwrap_or_else(|| placed.span(0));
                views.insert(
                    view.id.clone(),
                    View {
                        app: app.clone(),
                        name: view.name.clone(),
                        span,
                    },
                );
            }
            maps.insert(app, Package { calls });
        }
        Self {
            views,
            packages: maps,
        }
    }

    /// Where the node keyed `key` in a window's tree comes from, when its key is the compiler's.
    /// A key holds the keys of the items and row views it is in before its own, so the view
    /// named last is the node's.
    pub fn origin(&self, key: &str) -> Option<Origin> {
        let (at, view_id) = self
            .views
            .keys()
            .flat_map(|id| {
                key.match_indices(id.as_str()).filter_map(move |(at, _)| {
                    let before = key.get(..at)?.chars().next_back();
                    let after = key.get(at.saturating_add(id.len())..)?.chars().next();
                    (matches!(before, None | Some('~' | '.' | '/'))
                        && matches!(after, None | Some('~' | '.')))
                    .then_some((at, id))
                })
            })
            .max_by_key(|(at, _)| *at)?;
        let view = self.views.get(view_id)?;
        let rest = key
            .get(at.saturating_add(view_id.len())..)
            .unwrap_or_default();
        // A loop body is a row view of its own (`<view>~row<n>`), keyed after the view it is in.
        let rest = match rest.strip_prefix("~row") {
            Some(row) => row.trim_start_matches(|c: char| c.is_ascii_digit()),
            None => rest,
        };
        let rest = rest.strip_prefix('.').unwrap_or(rest);
        // A template's nodes are keyed after its call in this view: `t<node>.` for each call in.
        let node = match rest.strip_prefix('t') {
            Some(after) if after.contains('.') => leading_number(after),
            _ => leading_number(rest),
        };
        let call = node.and_then(|node| self.packages.get(&view.app)?.calls.get(&node));
        Some(Origin {
            app: view.app.clone(),
            view: view.name.clone(),
            node: call.map(|call| call.what.clone()),
            span: call.map_or_else(|| view.span.clone(), |call| call.span.clone()),
        })
    }

    /// Where a widget comes from, by the keys of it and the widgets around it, innermost
    /// first: the innermost that names a call in `app`'s own source, else the innermost in
    /// one of its views, else the innermost that resolves at all.
    pub fn origin_of(&self, app: &str, keys: &[String]) -> Option<Origin> {
        let origins: Vec<Origin> = keys.iter().filter_map(|key| self.origin(key)).collect();
        origins
            .iter()
            .find(|o| o.app == app && o.node.is_some())
            .or_else(|| origins.iter().find(|o| o.app == app))
            .or_else(|| origins.first())
            .cloned()
    }
}

fn leading_number(text: &str) -> Option<ast::NodeId> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}
