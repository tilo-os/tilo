//! What a window is sent of a view: its tree converted into what the window draws, the values
//! its bound controls hold, and how each reference in its rich text reads for the person — by
//! their permissions, their zone and language, and what the catalogue calls each resource now.
//!
//! Converting is also reading: what the host resolves for the window that the view did not read
//! (a card of a record inside the text it holds) is taken into the view's reads
//! ([`ViewInstance::shows`]), so a change to it runs the view again and the host converts again.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_render_protocol::patch::{self as ui, Lane, ToUi};

use super::rows;
use super::wire::{self, Convert};
use super::{Instance, cases};
use crate::host::Host;
use crate::runtime::{Binding, NameHost, Principal, ResourceRef, UiNode, Value, ViewInstance};

/// The window's tree of `view`, converted over `host`: what the window is sent, and what
/// converting it read of the workspace beside what the view read — the records the cards in its
/// text show — which the view then runs again on, so the host converts again.
#[must_use]
pub fn window_tree(
    host: &Host,
    names: &dyn NameHost,
    view: &Rc<RefCell<ViewInstance>>,
) -> tilo_render_protocol::patch::Node {
    convert_with(host, names, view, |convert, instance| {
        convert.node(instance.tree())
    })
}

/// Run `run` with the converter of `view`'s trees over `host`, naming people by `names`, and
/// tell the view what converting read of the workspace beside what it read (what a card shows of
/// the record it projects), so a change to one runs the view again and its tree is converted
/// again.
pub(crate) fn convert_with<T>(
    host: &Host,
    names: &dyn NameHost,
    view: &Rc<std::cell::RefCell<ViewInstance>>,
    run: impl FnOnce(&Convert<'_>, &ViewInstance) -> T,
) -> T {
    let field_name = |binding: &Binding| Instance::field_name(host, binding);
    let cases = |id: &str| cases(host, id);
    let embeds = |reference: &crate::runtime::rich::Reference| inline_view(host, names, reference);
    let link = |target: &ResourceRef| link_of(host, target);
    let (rendered, reads) = {
        let instance = view.borrow();
        let toggles = |key: &str, event: &str| toggles(host, &instance, key, event);
        let invokes = |key: &str, event: &str| invokes(host, &instance, key, event);
        let reads: std::cell::RefCell<Vec<(String, String)>> = std::cell::RefCell::new(Vec::new());
        let preview = |reference: &crate::runtime::rich::Reference| {
            preview(host, names, reference, &mut reads.borrow_mut())
        };
        let rendered = run(
            &Convert {
                names,
                field_name: &field_name,
                cases: &cases,
                toggles: &toggles,
                invokes: &invokes,
                embeds: &embeds,
                preview: &preview,
                link: &link,
            },
            &instance,
        );
        (rendered, reads.into_inner())
    };
    if !reads.is_empty()
        && let Ok(mut instance) = view.try_borrow_mut()
    {
        instance.shows(reads);
    }
    rendered
}

/// The tilolang resource a reference in rich text names, when it names one of a package this
/// host has loaded: by its id, of no type yet.
pub(crate) fn referenced_resource(
    host: &Host,
    reference: &crate::runtime::rich::Reference,
) -> Option<ResourceRef> {
    let at = reference.as_resource()?;
    host.package(&at.app)?;
    let id = crate::runtime::rich::resource_id(reference)?;
    ResourceRef::parse_id(id, String::new())
}

/// The catalogue's reference to the record `target` and how it reads for the viewer now: what a
/// component offering it as a link is given (`ui.BlockEditor`'s `links`). A type no package here
/// declares is named by its own id, as the record is found again by that.
#[must_use]
pub fn link_of(host: &Host, target: &ResourceRef) -> (String, String) {
    use crate::runtime::Store as _;
    let mut target = target.clone();
    if let Ok(ty) = host.store().type_of(&target) {
        target.ty = ty;
    }
    let title = host.label_at(&target);
    let reference = host
        .find_in_packages(|package| {
            package
                .meta
                .types
                .iter()
                .find(|declared| declared.id == target.ty)
                .map(|declared| {
                    crate::runtime::rich::resource_reference(
                        &package.meta.app,
                        &declared.name,
                        &target,
                    )
                    .mark_value()
                })
        })
        .unwrap_or_default();
    (reference, title)
}

/// How a reference in rich text reads for the viewer: a person by what the workspace calls them; a
/// resource of a package here by its title now, when the viewer holds it, and as only existing
/// when they do not (a reference never grants a read); a resource of another app as its run.
#[must_use]
pub fn inline_view(
    host: &Host,
    names: &dyn NameHost,
    reference: &crate::runtime::rich::Reference,
) -> tilo_render_protocol::patch::rich::InlineView {
    use crate::runtime::Store as _;
    use tilo_render_protocol::patch::rich::InlineView;
    if let Some(person) = reference.as_person() {
        return InlineView::Person {
            identity: person.to_owned(),
            name: names.name(&Principal::new(person)),
        };
    }
    let Some(mut target) = referenced_resource(host, reference) else {
        return InlineView::Elsewhere;
    };
    match host.store().type_of(&target) {
        Ok(ty) => {
            target.ty = ty;
            InlineView::Resource {
                title: host.label_at(&target),
                icon: None,
                detail: None,
            }
        }
        Err(_) => InlineView::Unreadable,
    }
}

/// The most of a projected record's content a card shows, in characters: the start of what it
/// says, the few lines a card has room for and some past them.
pub const PREVIEW_CHARS: usize = 320;

/// What the record `reference` shows of its own content where a text projects it (an embed):
/// the start of what it says, a paragraph per line it breaks, each reference in it read as it
/// reads now, one level deep — a projection inside it is a link, never opened out, so a note
/// embedding a note that embeds a note ends in a link, as a note embedding itself does. It is
/// [`PREVIEW_CHARS`] of it at most; a card shows the lines of that it has room for. Costs the
/// projected record's one read; `None` when it says nothing, or is a record this reader cannot
/// read.
#[must_use]
pub fn preview(
    host: &Host,
    names: &dyn NameHost,
    reference: &crate::runtime::rich::Reference,
    reads: &mut Vec<(String, String)>,
) -> Option<Vec<tilo_render_protocol::patch::rich::Run>> {
    use tilolang_runtime::Store as _;
    let mut target = referenced_resource(host, reference)?;
    target.ty = host.store().type_of(&target).ok()?;
    // What a card shows is read of the record: the view showing it runs again when it changes.
    reads.push((
        target.id_string(),
        tilolang_runtime::store::ANY_FIELD.to_owned(),
    ));
    reads.extend(
        crate::schema::lookup(&target.ty)
            .map(|schema| {
                schema
                    .fields
                    .iter()
                    .map(|field| (target.id_string(), field.id.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    let rich = first_rich(host, &target)?;
    let mut left = PREVIEW_CHARS;
    let mut runs: Vec<tilo_render_protocol::patch::rich::Run> = Vec::new();
    for block in rich
        .blocks
        .iter()
        .filter(|block| !block.text.trim().is_empty())
    {
        if left == 0 {
            break;
        }
        let said = block_runs(host, names, block);
        if said.is_empty() {
            continue;
        }
        // A paragraph of it stands under the one before, as it does in the record.
        if !runs.is_empty() {
            runs.push(runo("\n".to_owned(), None));
            left = left.saturating_sub(1);
        }
        for run in said {
            let count = run.text.chars().count();
            if count <= left {
                left = left.saturating_sub(count);
                runs.push(run);
                continue;
            }
            let kept: String = run.text.chars().take(left).collect();
            left = 0;
            if !kept.is_empty() {
                runs.push(runo(kept, run.reference));
            }
            break;
        }
    }
    Some(runs).filter(|runs| !runs.is_empty())
}

/// One paragraph of a projected record, as the runs a card shows of it: its text, a run per
/// reference in it, each read as it reads now.
fn block_runs(
    host: &Host,
    names: &dyn NameHost,
    block: &tilo_doc::snapshot::BlockView,
) -> Vec<tilo_render_protocol::patch::rich::Run> {
    let chars: Vec<char> = block.text.chars().collect();
    let written = |from: usize, to: usize| -> Option<String> {
        (from < to).then(|| chars.get(from..to).unwrap_or_default().iter().collect())
    };
    let mut runs: Vec<tilo_render_protocol::patch::rich::Run> = Vec::new();
    let mut at = 0usize;
    let mut marks: Vec<&tilo_doc::document::MarkSpan> = block
        .marks
        .iter()
        .filter(|mark| {
            mark.name == crate::runtime::rich::MENTION
                || mark.name == crate::runtime::rich::PROJECTION
        })
        .filter(|mark| mark.start < mark.end)
        .collect();
    marks.sort_by_key(|mark| (mark.start, std::cmp::Reverse(mark.end)));
    for mark in marks {
        let tilo_doc::document::MarkValue::Str(value) = &mark.value else {
            continue;
        };
        let (start, end) = (mark.start.min(chars.len()), mark.end.min(chars.len()));
        if start < at || start >= end {
            continue;
        }
        if let Some(text) = written(at, start) {
            runs.push(runo(text, None));
        }
        // What it names reads as it reads now, never as the title its run kept.
        let refers_to = crate::runtime::rich::Reference::of_mark(value);
        let title = refers_to
            .as_ref()
            .map(|reference| inline_title(host, names, reference))
            .filter(|title| !title.is_empty());
        let (text, refers) = match (title, written(start, end), refers_to) {
            (Some(title), _, refers_to) => {
                let refers = refers_to
                    .filter(|reference| reference.as_resource().is_some())
                    .map(|_| value.clone());
                (title, refers)
            }
            (None, Some(written), _) => (written, None),
            (None, None, _) => continue,
        };
        runs.push(runo(text, refers));
        at = end;
    }
    if let Some(text) = written(at, chars.len()) {
        runs.push(runo(text, None));
    }
    runs
}

/// One run of a projection's content, naming `refers` when it refers to something.
fn runo(text: String, refers: Option<String>) -> tilo_render_protocol::patch::rich::Run {
    tilo_render_protocol::patch::rich::Run {
        text,
        reference: refers,
    }
}

/// What the record `reference` reads as, a title and nothing else: what a projected run says of
/// what it names.
#[must_use]
pub fn inline_title(
    host: &Host,
    names: &dyn NameHost,
    reference: &crate::runtime::rich::Reference,
) -> String {
    match inline_view(host, names, reference) {
        tilo_render_protocol::patch::rich::InlineView::Resource { title, .. }
            if !title.is_empty() =>
        {
            title
        }
        tilo_render_protocol::patch::rich::InlineView::Person { name, .. } if !name.is_empty() => {
            name
        }
        tilo_render_protocol::patch::rich::InlineView::Unreadable => {
            crate::runtime::rich::UNSHARED.to_owned()
        }
        _ => String::new(),
    }
}

/// The first rich text of the record at `at`: the content it says of itself, read as the
/// record's own fields are.
pub(crate) fn first_rich(host: &Host, at: &ResourceRef) -> Option<crate::runtime::rich::RichValue> {
    use tilolang_runtime::Store as _;
    let schema = crate::schema::lookup(&at.ty)?;
    let field = schema
        .fields
        .iter()
        .find(|field| field.kind == crate::schema::MergeKind::RichText)?;
    let spec = tilolang_runtime::store::FieldSpec {
        id: field.id.clone(),
        kind: tilolang_runtime::meta::FieldKind::RichText,
    };
    match host.store().read(at, &spec).ok()? {
        tilolang_runtime::store::FieldValue::Rich(rich) => Some(rich),
        _ => None,
    }
}

/// The values the bound controls of `view`'s tree hold now, by the control's key: what a window
/// is sent as each control's value, converted as it reads for the person — a rich text's
/// references and the records its cards show resolved now.
#[must_use]
pub fn shown_values(
    host: &Host,
    names: &dyn NameHost,
    view: &Rc<RefCell<ViewInstance>>,
) -> Vec<(String, ui::Value)> {
    let mut now: Vec<(String, ui::Value)> = Vec::new();
    convert_with(host, names, view, |convert, instance| {
        bound_values(instance.tree(), instance, convert, &mut now);
    });
    now
}

/// The values the bound controls under `node` (a node of `view`) hold now, converted as what a
/// control shows of them: a rich text's references and the records its cards show as they read
/// for the person, which converting reads and the view watches.
pub(crate) fn bound_values(
    node: &UiNode,
    view: &ViewInstance,
    convert: &Convert<'_>,
    out: &mut Vec<(String, ui::Value)>,
) {
    if node.binding.is_some()
        && let Some(value) = view.bound_value(&node.key)
    {
        out.push((node.key.clone(), convert.value(&value)));
    }
    for child in &node.children {
        if !ui::is_virtual_list(&node.kind) || child.kind == wire::SLOT {
            bound_values(child, view, convert, out);
        }
    }
}

/// The resource and field the event `event` of the node `key` of `view` toggles, when the
/// action it applies toggles one (`pin` toggles `pinned`).
pub(crate) fn toggles(
    host: &Host,
    view: &ViewInstance,
    key: &str,
    event: &str,
) -> Option<(ResourceRef, String)> {
    let aref = view.event_action(key, event)?;
    let field = host.find_in_packages(|package| {
        package
            .meta
            .actions
            .iter()
            .find(|action| action.id == aref.action)
            .map(|action| action.toggles.clone())
    })??;
    match aref.args.first() {
        Some(Value::Ref(subject)) => Some((ResourceRef::clone(&subject.target), field)),
        _ => None,
    }
}

/// The resource and the action the event `event` of the node `key` of `view` runs, when it
/// runs an action on a resource as it is: the action its control is bound to. An action that
/// still asks for something (`Pick date…`) opens its form from the view's own event instead.
pub(crate) fn invokes(
    host: &Host,
    view: &ViewInstance,
    key: &str,
    event: &str,
) -> Option<(ResourceRef, String)> {
    // What the control does, where a `then` runs more than one action to do it.
    let aref = view.event_action(key, event)?.acting().clone();
    let given = aref
        .args
        .iter()
        .position(|arg| *arg == Value::Missing)
        .unwrap_or(aref.args.len());
    let asks = host.find_in_packages(|package| {
        package
            .meta
            .actions
            .iter()
            .find(|action| action.id == aref.action)
            .map(|action| action.unbound(given).next().is_some())
    })?;
    if asks {
        return None;
    }
    match aref.args.first() {
        Some(Value::Ref(subject)) => Some((ResourceRef::clone(&subject.target), aref.action)),
        _ => None,
    }
}

/// The window's message of rows `requested` of the list `list` of the view `view` of `lane`.
pub(crate) fn rows_message(
    host: &Host,
    names: &dyn NameHost,
    view: &Rc<RefCell<ViewInstance>>,
    lane: Lane,
    list: &str,
    requested: rows::Requested,
) -> ToUi {
    let rows = convert_with(host, names, view, |convert, _| {
        requested.rows.iter().map(|row| convert.node(row)).collect()
    });
    ToUi::Rows {
        lane,
        list: list.to_owned(),
        len: u32::try_from(requested.len).unwrap_or(u32::MAX),
        start: u32::try_from(requested.start).unwrap_or(u32::MAX),
        keys: requested.keys,
        rows,
    }
}
