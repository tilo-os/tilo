//! The tilolang shell (`apps/tl/shell`) as every host runs it: its package, compiled from the
//! sources this build ships ([`compile`]), and the person's layout written into it as the
//! origin of its `session.*` resources ([`LayoutWriter`]). A native session draws it under the
//! windows; a server shows a workspace's canvas through it on a web page. Both compile it and
//! write its layout here, so the two show the same layout the same way.

use std::collections::BTreeMap;

/// The person's layout as a host hands it to [`LayoutOrigin`]: what the shell's views read.
pub use tilo_desktop_model::reading::{LayoutReading, LayoutWorkspaceReading};
use tilo_desktop_model::reading::{
    LayoutPersonReading, LayoutPlaceReading, LayoutQueryReading, LayoutRecentReading,
    LayoutRefReading, LayoutRowReading, LayoutStripReading,
};

use crate::origin::{OriginCx, OriginType, origin_types};
use crate::runtime::origin::OriginWrite;
use crate::runtime::{ResourceRef, Value};
use crate::tilolang::Compiled;

mod asking;
pub use asking::{AskedReading, AskingOrigin, AskingWriter};

/// The shell's package id.
pub const SHELL_APP: &str = "so.tilo.shell";

/// The origin of the shell's `session.*` resources: what its `Layout` type names
/// (`type Layout (origin: session.layout)`).
pub const ORIGIN: &str = "session";

/// Every file of the shell's package (`apps/tl/shell`) the session runs, by the name the
/// package knows it by: a package is a directory of `.tl` files, in path order (the package's
/// compiler reads them so), and each new one is listed here to ship. Its scenarios
/// (`scenarios.tl`), which stage the shell over other apps' records for its shots and tests,
/// are fixtures: the session never runs them, so it compiles the shell without them and needs
/// no other app's package to do so.
pub const SHELL_FILES: &[(&str, &str)] = &[
    ("app.tl", include_str!("../../../../apps/tl/shell/app.tl")),
    (
        "asking.tl",
        include_str!("../../../../apps/tl/shell/asking.tl"),
    ),
    (
        "canvas.tl",
        include_str!("../../../../apps/tl/shell/canvas.tl"),
    ),
    (
        "chrome.tl",
        include_str!("../../../../apps/tl/shell/chrome.tl"),
    ),
    (
        "docks.tl",
        include_str!("../../../../apps/tl/shell/docks.tl"),
    ),
    (
        "feedback.tl",
        include_str!("../../../../apps/tl/shell/feedback.tl"),
    ),
    (
        "history.tl",
        include_str!("../../../../apps/tl/shell/history.tl"),
    ),
    (
        "launcher.tl",
        include_str!("../../../../apps/tl/shell/launcher.tl"),
    ),
    (
        "layout.tl",
        include_str!("../../../../apps/tl/shell/layout.tl"),
    ),
    (
        "layout_picker.tl",
        include_str!("../../../../apps/tl/shell/layout_picker.tl"),
    ),
    (
        "notices.tl",
        include_str!("../../../../apps/tl/shell/notices.tl"),
    ),
    ("osd.tl", include_str!("../../../../apps/tl/shell/osd.tl")),
    (
        "overview.tl",
        include_str!("../../../../apps/tl/shell/overview.tl"),
    ),
    (
        "parts.tl",
        include_str!("../../../../apps/tl/shell/parts.tl"),
    ),
    (
        "people.tl",
        include_str!("../../../../apps/tl/shell/people.tl"),
    ),
    (
        "phone.tl",
        include_str!("../../../../apps/tl/shell/phone.tl"),
    ),
    (
        "presets.tl",
        include_str!("../../../../apps/tl/shell/presets.tl"),
    ),
    (
        "quick.tl",
        include_str!("../../../../apps/tl/shell/quick.tl"),
    ),
    (
        "session.tl",
        include_str!("../../../../apps/tl/shell/session.tl"),
    ),
    (
        "sheets.tl",
        include_str!("../../../../apps/tl/shell/sheets.tl"),
    ),
    (
        "shell.tl",
        include_str!("../../../../apps/tl/shell/shell.tl"),
    ),
    (
        "strips.tl",
        include_str!("../../../../apps/tl/shell/strips.tl"),
    ),
    (
        "style_settings.tl",
        include_str!("../../../../apps/tl/shell/style_settings.tl"),
    ),
    (
        "switcher.tl",
        include_str!("../../../../apps/tl/shell/switcher.tl"),
    ),
    (
        "traditional.tl",
        include_str!("../../../../apps/tl/shell/traditional.tl"),
    ),
    (
        "workspaces.tl",
        include_str!("../../../../apps/tl/shell/workspaces.tl"),
    ),
];
/// The shell package's ids lock.
const SHELL_IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");

/// Every file of the shell package this build ships, by the name the package knows it by
/// (`app.tl`, `strips.tl`, …) and its source: the same copy [`compile`] compiles, so
/// "make it mine" copies exactly the style a person is looking at (`shell.md` §6). Its
/// scenarios (`scenarios.tl`) are not here: they are fixtures, never part of the package a
/// session runs.
#[must_use]
pub const fn sources() -> &'static [(&'static str, &'static str)] {
    SHELL_FILES
}

/// The catalogues the shell package ships, by locale: its words in the languages a window may
/// read them in, travelling in its module as an installed app's do.
#[must_use]
pub const fn catalogues() -> [(&'static str, &'static str); 2] {
    [
        (
            "en-US",
            include_str!("../../../../apps/tl/shell/locales/en-US.ftl"),
        ),
        (
            "de-DE",
            include_str!("../../../../apps/tl/shell/locales/de-DE.ftl"),
        ),
    ]
}

/// The shell package, compiled: what a shell's instance loads, over the session's and the
/// platform's descriptions. No other app's package is the shell's: the package of each app a
/// place shows comes from its host when a place shows it.
/// `Err` when it does not compile, which every build of the shell is checked against, so a
/// test that fails to compile here fails loudly.
///
/// This is the fallback shell, for a device whose app store holds no `so.tilo.shell` (or one
/// that does not load), and for a host that keeps no app store (a server's web page).
///
/// # Errors
/// The package's diagnostics, rendered with their file names.
pub fn compile() -> Result<Vec<Compiled>, String> {
    let session = crate::platform_apps::session_package()?;
    let mut out: Vec<Compiled> = Vec::new();
    let lock = crate::tilolang::IdsLock::parse(SHELL_IDS).map_err(|e| e.to_string())?;
    let source = crate::tilolang::PackageSource::new(
        SHELL_FILES
            .iter()
            .map(|(path, text)| (format!("apps/tl/shell/{path}"), (*text).to_owned())),
    )
    // The shell's own words travel in its module, as an installed app's do.
    .with_locales(
        catalogues()
            .into_iter()
            .map(|(locale, text)| (locale.to_owned(), text.to_owned())),
    );
    let mut imports: Vec<crate::tilolang::PackageMeta> = crate::platform_apps::platform_packages()?
        .into_iter()
        .filter(|meta| meta.app != crate::platform_apps::SESSION_APP)
        .collect();
    imports.insert(0, session);
    let shell = crate::tilolang::compile_package_source(&source, &lock, &imports).map_err(
        |diagnostics| {
            diagnostics
                .iter()
                .map(|d| d.render(d.file.as_deref().unwrap_or("apps/tl/shell/app.tl")))
                .collect::<Vec<_>>()
                .join("\n")
        },
    )?;
    out.push(shell);
    Ok(out)
}
/// What the origin last wrote of an item kept by key (a workspace, a task).
struct Kept {
    /// Where the insert made it; `None` until the write that inserts it is applied.
    at: Option<ResourceRef>,
    /// The fields it was written with.
    was: Vec<(String, Value)>,
    /// When it was inserted, among every item the origin inserted: its collection holds its
    /// items in that order.
    seq: u64,
}

/// What the origin wrote into the shell's resources: the fields of a root and the items of its
/// collections, by collection and key, and what the writes being applied insert and set.
#[derive(Default)]
pub struct Mirror {
    kept: BTreeMap<(String, String), Kept>,
    fields: BTreeMap<String, Value>,
    inserting: Vec<(String, String)>,
    setting: Vec<(String, Value)>,
    inserted: u64,
}

impl Mirror {
    /// The write that sets the fields of `root` that changed since they were last written.
    pub fn set(
        &mut self,
        root: &ResourceRef,
        now: impl IntoIterator<Item = (String, Value)>,
    ) -> Option<OriginWrite> {
        let fields: Vec<(String, Value)> = now
            .into_iter()
            .filter(|(id, value)| {
                self.fields.get(&format!("{}#{id}", root.id_string())) != Some(value)
            })
            .collect();
        self.setting.extend(
            fields
                .iter()
                .map(|(id, value)| (format!("{}#{id}", root.id_string()), value.clone())),
        );
        (!fields.is_empty()).then(|| OriginWrite::Set {
            target: root.clone(),
            fields,
        })
    }

    /// What became of the writes just applied: where each insert made its item (one entry per
    /// write, `None` for every write but an insert), or `None` when nothing was written.
    pub fn made(&mut self, made: Option<Vec<Option<ResourceRef>>>) {
        let setting = std::mem::take(&mut self.setting);
        if made.is_some() {
            self.fields.extend(setting);
        }
        let mut made = made.into_iter().flatten().flatten();
        for item in std::mem::take(&mut self.inserting) {
            match made.next() {
                Some(at) => {
                    if let Some(kept) = self.kept.get_mut(&item) {
                        kept.at = Some(at);
                    }
                }
                None => {
                    self.kept.remove(&item);
                }
            }
        }
    }
}

impl Mirror {
    /// Apply `writes` as the origin's one change, keeping what became of them. Whether it was
    /// written; `what` names it where it could not be.
    pub fn apply(&mut self, cx: &OriginCx<'_>, writes: Vec<OriginWrite>, what: &str) -> bool {
        match cx
            .host
            .runtime()
            .apply_origin(ORIGIN, writes, Some(cx.now.clone()))
        {
            Ok(made) => {
                self.made(Some(made));
                true
            }
            Err(error) => {
                self.made(None);
                tracing::error!(%error, "{what} could not be written");
                false
            }
        }
    }
}

/// The root the shell's type of the origin's kind `kind` (`layout`) is, `root-<type id>`, made
/// the first time it is wanted as the shell's `root` declaration reads it, with the shell's
/// types of the origin's kinds; `None` where the shell declares no such type.
fn origin_root(cx: &OriginCx<'_>, kind: &str) -> Option<(ResourceRef, Vec<OriginType>)> {
    let Some(types) = cx.host.find_in_packages(|package| {
        (package.meta.app == cx.app).then(|| origin_types(&package.meta, ORIGIN))
    }) else {
        tracing::error!(app = cx.app, "the shell's package is not loaded");
        return None;
    };
    let ty = types
        .iter()
        .find(|ty| ty.kind == format!("{ORIGIN}.{kind}"))?
        .ty
        .clone();
    let made = ResourceRef::root(format!("root-{ty}"), ty.clone());
    let root = if cx.host.store().contains(&made) {
        made
    } else {
        match cx.host.runtime().create_root(&ty) {
            Ok(root) => root,
            Err(error) => {
                tracing::error!(%error, kind, "the shell has no root to write");
                return None;
            }
        }
    };
    Some((root, types))
}

impl Mirror {
    /// The writes that bring the items of `collection` on `root` from what was written to `now`,
    /// by key: new ones inserted (where, [`Self::made`] keeps), gone ones removed, changed ones
    /// set. A collection holds its items in the order they were inserted and has no move, so
    /// where `now` orders them otherwise (a place moved to the end of another strip, a tab ranked
    /// anew) the items from the first one out of order on are taken out and inserted again.
    pub fn sync<T>(
        &mut self,
        now: &[T],
        root: &ResourceRef,
        collection: Option<&str>,
        ty: Option<&OriginType>,
        key: impl Fn(&T) -> String,
        fields: impl Fn(&T) -> Vec<(&'static str, Value)>,
    ) -> Vec<OriginWrite> {
        // A shell whose package declares no such collection is written none of it.
        let (Some(collection), Some(ty)) = (collection, ty) else {
            return Vec::new();
        };
        let named = |item: &T| -> Vec<(String, Value)> {
            fields(item)
                .into_iter()
                .filter_map(|(name, value)| Some((ty.field(name)?.to_owned(), value)))
                .collect()
        };
        let here = format!("{}#{collection}", root.id_string());
        let keys: Vec<String> = now.iter().map(&key).collect();
        let mut held: Vec<(u64, String)> = self
            .kept
            .iter()
            .filter(|((owner, _), _)| *owner == here)
            .map(|((_, key), item)| (item.seq, key.clone()))
            .collect();
        held.sort_unstable();
        let ordered = keys
            .iter()
            .zip(&held)
            .take_while(|(now, (_, held))| *now == held)
            .count();
        // Gone, or out of order: taken out, to be inserted again where it stands now.
        let mut writes = Vec::new();
        for (_, gone) in held {
            let stays = keys.iter().take(ordered).any(|key| *key == gone);
            if stays {
                continue;
            }
            if let Some(Kept { at: Some(item), .. }) = self.kept.remove(&(here.clone(), gone)) {
                writes.push(OriginWrite::Remove {
                    owner: root.clone(),
                    field: collection.to_owned(),
                    item,
                });
            }
        }
        for (item, key) in now.iter().zip(keys) {
            let fields = named(item);
            let at = (here.clone(), key);
            match self.kept.get_mut(&at) {
                Some(kept) if kept.was == fields => {}
                Some(kept) => {
                    if let Some(target) = kept.at.clone() {
                        writes.push(OriginWrite::Set {
                            target,
                            fields: fields.clone(),
                        });
                    }
                    kept.was = fields;
                }
                None => {
                    writes.push(OriginWrite::Insert {
                        owner: root.clone(),
                        field: collection.to_owned(),
                        ty: ty.ty.clone(),
                        fields: fields.clone(),
                    });
                    self.inserted = self.inserted.wrapping_add(1);
                    self.kept.insert(
                        at.clone(),
                        Kept {
                            at: None,
                            was: fields,
                            seq: self.inserted,
                        },
                    );
                    self.inserting.push(at);
                }
            }
        }
        writes
    }
}

/// The person's layout as it was last written into a shell's `Layout` root, so the next write
/// changes only what moved: a place another member dragged is one set of its strip's point.
#[derive(Default)]
pub struct LayoutWriter {
    mirror: Mirror,
}

/// The field value a text is, in the shell's record.
fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

impl LayoutWriter {
    /// Write `reading` into the shell's `Layout` root: only what changed since the last write.
    /// Whether anything was written.
    pub fn write(&mut self, cx: &OriginCx<'_>, reading: &LayoutReading) -> bool {
        // A shell without a layout view keeps its bar alone: nothing to write.
        let Some((root, types)) = origin_root(cx, "layout") else {
            return false;
        };
        let writes = self.layout_writes(reading, &root, &types);
        self.mirror.apply(cx, writes, "the person's layout")
    }

    /// The writes that bring the shell's layout root from what was written to `reading`.
    fn layout_writes(
        &mut self,
        reading: &LayoutReading,
        root: &ResourceRef,
        types: &[OriginType],
    ) -> Vec<OriginWrite> {
        let find = |name: &str| {
            types
                .iter()
                .find(|ty| ty.kind == format!("{ORIGIN}.{name}"))
        };
        let Some(ty) = find("layout") else {
            return Vec::new();
        };
        let person = ty
            .field("person")
            .map(|field| (field.to_owned(), text(&reading.person)));
        // The window in front is this device's, read off its shell: the shell frames that one as
        // focused, and a press on another asks the session to bring it forward.
        let front = ty
            .field("front")
            .map(|field| (field.to_owned(), text(&reading.front)));
        let mut writes: Vec<OriginWrite> = self
            .mirror
            .set(root, person.into_iter().chain(front))
            .into_iter()
            .collect();
        // Each collection of the root, from the reading's items of its kind.
        macro_rules! sync {
            ($items:expr, $field:literal, $kind:literal, $fields:ident) => {
                writes.extend(self.mirror.sync(
                    &$items,
                    root,
                    ty.field($field),
                    find($kind),
                    |item| item.key.clone(),
                    $fields,
                ));
            };
        }
        sync!(
            reading.workspaces,
            "workspaces",
            "layout_workspace",
            workspace_fields
        );
        sync!(reading.people, "people", "layout_person", person_fields);
        sync!(reading.rows, "rows", "layout_row", row_fields);
        sync!(reading.strips, "strips", "layout_strip", strip_fields);
        sync!(reading.places, "places", "layout_place", place_fields);
        // The refs each query found live in their own collection, keyed by their `key`
        // (as `<query>/<number>`), so a query's rows are the refs that name it.
        sync!(reading.queries, "queries", "layout_query", query_fields);
        let refs: Vec<LayoutRefReading> = reading
            .queries
            .iter()
            .flat_map(|query| query.matches.iter().cloned())
            .collect();
        sync!(refs, "refs", "layout_ref", ref_fields);
        sync!(reading.recents, "recents", "layout_recent", recent_fields);
        writes
    }
}

/// The fields of a canvas query, as it is written into the shell's record. Its refs are the
/// `refs` collection's, each naming its query by key.
fn query_fields(q: &LayoutQueryReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", Value::Text(q.key.clone())),
        ("text", Value::Text(q.text.clone())),
        ("in_app", Value::Text(q.app.clone())),
        ("kind", Value::Text(q.kind.clone())),
        ("state", Value::Text(q.state.clone())),
        ("reason", Value::Text(q.reason.clone())),
    ]
}

/// The fields of a resource a query found, as it is written into the shell's record: its query by
/// key, its address, its title and its kind.
fn ref_fields(r: &LayoutRefReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", Value::Text(r.key.clone())),
        ("query", Value::Text(query_of_ref_key(&r.key))),
        ("address", Value::Text(r.address.clone())),
        ("title", Value::Text(r.title.clone())),
        ("kind", Value::Text(r.kind.clone())),
    ]
}

/// The fields of a resource of the person's recents, as it is written into the shell's record.
fn recent_fields(r: &LayoutRecentReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", Value::Text(r.key.clone())),
        ("resource", Value::Text(r.resource.clone())),
        ("title", Value::Text(r.title.clone())),
        ("origin_app", Value::Text(r.origin_app.clone())),
        ("app_title", Value::Text(r.app_title.clone())),
        ("kind", Value::Text(r.kind.clone())),
        ("by", Value::Text(r.by.clone())),
        (
            "at",
            Value::Text(crate::tilolang::calendar::time_text(r.at)),
        ),
        ("mine", Value::Bool(r.mine)),
    ]
}

/// The query a ref belongs to, from its key (`<query>/<number>`).
fn query_of_ref_key(key: &str) -> String {
    key.split('/').next().unwrap_or_default().to_owned()
}

/// The fields of a layout workspace, as it is written into the shell's record.
fn workspace_fields(w: &LayoutWorkspaceReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&w.key)),
        ("name", text(&w.name)),
        ("current", Value::Bool(w.current)),
        ("home", text(&w.home)),
    ]
}

/// The fields of a row, as it is written into the shell's record.
fn row_fields(r: &LayoutRowReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&r.key)),
        ("workspace", text(&r.workspace)),
        ("name", text(&r.name)),
        ("shell", text(&r.shell)),
        ("current", Value::Bool(r.current)),
    ]
}

/// The fields of someone here, as they are written into the shell's record.
fn person_fields(p: &LayoutPersonReading) -> Vec<(&'static str, Value)> {
    vec![("key", text(&p.key)), ("name", text(&p.name))]
}

/// The fields of a strip, as it is written into the shell's record.
fn strip_fields(s: &LayoutStripReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&s.key)),
        ("workspace", text(&s.workspace)),
        ("row", text(&s.row)),
        ("width", Value::Int(s.width)),
        ("maximized", Value::Bool(s.maximized)),
        ("active", text(&s.active)),
        ("placed", Value::Bool(s.placed)),
        ("canvas_x", Value::Int(s.canvas_x)),
        ("canvas_y", Value::Int(s.canvas_y)),
        ("canvas_width", Value::Int(s.canvas_width)),
        ("canvas_height", Value::Int(s.canvas_height)),
        ("canvas_hugs", Value::Bool(s.canvas_hugs)),
        ("shell", text(&s.shell)),
    ]
}

/// The fields of a place, as it is written into the shell's record.
fn place_fields(p: &LayoutPlaceReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&p.key)),
        ("strip", text(&p.strip)),
        ("shows", text(&p.shows)),
        ("shown_as", text(&p.shown_as)),
        ("resource", text(&p.resource)),
        ("title", text(&p.title)),
        ("name", text(&p.name)),
        ("pinned", Value::Bool(p.pinned)),
        ("shell", text(&p.shell)),
    ]
}

/// The person's layout as the origin of a shell's `session.*` resources, for a host that has
/// nothing else of the session to give it: a server showing a workspace's canvas on a web page.
/// The host hands it each reading ([`Self::read`]); the instance writes what changed with its
/// next frame.
#[derive(Default)]
pub struct LayoutOrigin {
    writer: LayoutWriter,
    /// The last reading handed in.
    reading: Option<LayoutReading>,
    /// Whether that reading is still to be written.
    due: bool,
}

impl LayoutOrigin {
    /// The person's layout now, written with the instance's next frame when it changed.
    /// Whether it changed.
    pub fn read(&mut self, reading: LayoutReading) -> bool {
        if self.reading.as_ref() == Some(&reading) {
            return false;
        }
        self.reading = Some(reading);
        self.due = true;
        true
    }
}

impl crate::origin::Origin for LayoutOrigin {
    fn name(&self) -> &str {
        ORIGIN
    }

    /// The shell's views call the session's actions as an app's; no outside effect is the
    /// layout's.
    fn serves(&self, _capability: &str) -> bool {
        false
    }

    fn due(&self) -> bool {
        self.due
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> crate::origin::Polled {
        self.due = false;
        let wrote = match &self.reading {
            Some(reading) => self.writer.write(cx, reading),
            None => false,
        };
        crate::origin::Polled {
            wrote,
            settle: false,
        }
    }

    fn deliver(
        &mut self,
        capability: &str,
        _payload: &serde_json::Value,
    ) -> tilolang_runtime::Delivery {
        tilolang_runtime::Delivery::Rejected(format!(
            "the layout takes no outside effect `{capability}`"
        ))
    }
}
