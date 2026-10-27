//! The operations a planning model is shown with its request
//! ([`catalogue_summary`]): enough to plan the common ones without a
//! discovery round trip, each application with its share of the cap, and —
//! for a planner whose records keep publishing resources as a task runs
//! ([`catalogue_summary_with`]) — look-alike resources in one line and the
//! resources the request names listed whatever the cap.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::{
    HOST_APP_PREFIX, LIST_APPLICATIONS, LIST_OPERATIONS, Revealed, confirmation_note, open_note,
    opened_app, parameter_signature,
};
use crate::surface::{Catalogue, Offer};

/// How many operations the first turn's catalogue shows.
pub const MOST_SUMMARISED: usize = 60;

/// The published operations, compactly, for the first turn (of the task loop
/// and of a plan decision): enough to invoke or plan the common ones directly
/// without a discovery round trip. Every id shown is revealed. Every
/// running application gets its share (document operations first, then
/// view, then the rest) of the first half; the opens — how a closed
/// application is started — follow, Tilo's applications first; a cut
/// summary says how to reach what it left out.
pub fn catalogue_summary(catalogue: &Catalogue, revealed: &mut Revealed, most: usize) -> String {
    catalogue_summary_with(catalogue, revealed, most, None)
}

/// The fewest characters of a resource's own id (what follows its kind) a
/// text must hold for it to name that resource: a uuid or a path, never a
/// short index any number in the text could match.
const MIN_NAMED_ID_CHARS: usize = 8;

/// The fewest characters of a resource's label a text must hold for it to
/// name that resource.
const MIN_NAMED_LABEL_CHARS: usize = 3;

/// How many resources of one application and kind with the same operations
/// a [`catalogue_summary_with`] lists one by one; this many or more share
/// one line.
pub const MOST_LISTED_ALIKE: usize = 5;

/// The resources a request names, read from its text: by a resource's own
/// id (one an earlier step returned, in what the steps answered) or by its
/// label (a channel, a sheet, a clip the person named).
#[derive(Clone, Debug)]
pub struct Naming {
    /// The text, lowercased.
    text: String,
}

impl Naming {
    /// The resources `text` names.
    pub fn in_text(text: &str) -> Self {
        Self {
            text: text.to_lowercase(),
        }
    }

    /// Whether the text names the resource `resource` by its own id: what
    /// follows its kind's `:` (`files.entry:/home/q/a.png`), or the last
    /// part of an item's address (`sheets.workbook/sheets/<id>`). A
    /// resource without one (a document's root) is never named by its id.
    fn names_resource(&self, resource: &str) -> bool {
        let id = resource
            .split_once(':')
            .map(|(_, id)| id)
            .or_else(|| resource.rsplit_once('/').map(|(_, id)| id))
            .map(str::trim)
            .unwrap_or_default();
        id.chars().count() >= MIN_NAMED_ID_CHARS && self.text.contains(&id.to_lowercase())
    }

    /// Whether the text names a resource by `label`. A leading `#` is
    /// ignored: a channel's label carries Chat's, which a request naming
    /// the channel ("reply in the Lena channel") leaves out.
    fn names_label(&self, label: &str) -> bool {
        let label = label.trim().trim_start_matches('#').trim_start();
        label.chars().count() >= MIN_NAMED_LABEL_CHARS && self.text.contains(&label.to_lowercase())
    }
}

/// A resource of the catalogue: its application and id.
type ResourceKey<'c> = (&'c str, &'c str);

/// The application and resource `offer` acts on.
fn resource_key(offer: &Offer) -> ResourceKey<'_> {
    (offer.app.as_str(), offer.address().resource.as_str())
}

/// [`catalogue_summary`], reading what a plan's request names. With
/// `named`, resources of one application and kind with the same operations,
/// [`MOST_LISTED_ALIKE`] or more (a timeline's clips, a workbook's sheets),
/// are counted once against `most` and said in one line, so one kind a
/// task keeps adding to cannot crowd the rest out; and every resource
/// `named` names is listed on top of the applications' shares, whatever the
/// cap, so an id an earlier step returned is one the model may plan with.
pub fn catalogue_summary_with(
    catalogue: &Catalogue,
    revealed: &mut Revealed,
    most: usize,
    named: Option<&Naming>,
) -> String {
    let rank = |offer: &&Offer| match offer.descriptor.kind {
        _ if opened_app(offer).is_some() => 0,
        tilo_actions::OperationKind::Document => 1,
        tilo_actions::OperationKind::View => 2,
        tilo_actions::OperationKind::Effect => 3,
    };
    let groups = if named.is_some() {
        alike_groups(catalogue)
    } else {
        BTreeMap::new()
    };
    // The group each resource is in, by its head, which stands for all of
    // it until the cap is applied.
    let head_of: BTreeMap<ResourceKey<'_>, ResourceKey<'_>> = groups
        .iter()
        .flat_map(|(head, rest)| rest.iter().map(|member| (*member, *head)))
        .collect();
    // Per application, best first; then round-robin so a large application
    // cannot crowd the others (or the opens) out of the summary.
    let mut per_app: BTreeMap<&str, Vec<&Offer>> = BTreeMap::default();
    for offer in &catalogue.offers {
        let offers = per_app.entry(offer.app.as_str()).or_default();
        if !head_of.contains_key(&resource_key(offer)) {
            offers.push(offer);
        }
    }
    for offers in per_app.values_mut() {
        offers.sort_by_key(rank);
    }
    let mut chosen = summarised(catalogue, &per_app, most);
    if let Some(named) = named {
        chosen.extend(named_offers(catalogue, named, &head_of, &chosen));
    }
    let shown = with_alike(catalogue, &head_of, chosen);
    if shown.is_empty() {
        return "No application publishes an operation you may perform right now.\n".into();
    }
    // Grouped by application, then by resource in first-appearance order;
    // a member of an alike group is said with its head.
    let mut resources: Vec<ResourceKey<'_>> = Vec::new();
    for app in per_app.keys() {
        for offer in shown.iter().filter(|o| o.app == *app) {
            let key = resource_key(offer);
            if !head_of.contains_key(&key) && !resources.contains(&key) {
                resources.push(key);
            }
        }
    }
    let mut s = String::new();
    for (app, resource) in resources {
        let offers: Vec<&Offer> = shown
            .iter()
            .copied()
            .filter(|o| resource_key(o) == (app, resource))
            .collect();
        let Some(head) = offers.first() else {
            continue;
        };
        if let Some(members) = groups.get(&(app, resource)) {
            write_alike(&mut s, catalogue, head, members, &offers);
            for offer in shown
                .iter()
                .filter(|o| head_of.get(&resource_key(o)) == Some(&(app, resource)))
            {
                revealed.reveal(offer.id());
            }
        } else {
            let _ = writeln!(
                s,
                "In {app}: {} ({}) — resource {resource}",
                head.descriptor.resource_label, head.descriptor.resource_kind
            );
            for offer in &offers {
                s.push_str(&summary_line(catalogue, offer));
            }
        }
        for offer in &offers {
            revealed.reveal(offer.id());
        }
    }
    let rest = catalogue.offers.len().saturating_sub(shown.len());
    if rest > 0 {
        let _ = writeln!(
            s,
            "… and {rest} more; ask `{LIST_OPERATIONS}` (by application, kind, resource or \
             search) to reach them, or `{LIST_APPLICATIONS}` for the applications there are."
        );
    }
    s
}

/// One offer's line in a summary: its id, label, what an open does, why it
/// is unavailable, whether the person is asked first, and its parameters.
fn summary_line(catalogue: &Catalogue, offer: &Offer) -> String {
    let mut line = format!("  {} — {}", offer.id(), offer.descriptor.action_label);
    if let Some(opened) = opened_app(offer) {
        line.push_str(&open_note(catalogue, offer, opened));
    }
    if let tilo_actions::Availability::Unavailable { reason, .. } =
        &offer.descriptor.contract.availability
    {
        let _ = write!(line, " (currently unavailable: {reason})");
    }
    if let Some(note) = confirmation_note(offer) {
        let _ = write!(line, " ({note})");
    }
    let signature = parameter_signature(offer);
    if !signature.is_empty() {
        let _ = write!(line, " {signature}");
    }
    line.push('\n');
    line
}

/// An alike group in one line for all its resources, then the operations
/// its `head` was listed with (`offers`), each once, named as
/// `<resource>#operation`.
fn write_alike(
    s: &mut String,
    catalogue: &Catalogue,
    head: &Offer,
    members: &[ResourceKey<'_>],
    offers: &[&Offer],
) {
    let (app, resource) = resource_key(head);
    let label = |resource: &str| {
        catalogue
            .offers
            .iter()
            .find(|offer| offer.app == app && offer.address().resource == resource)
            .map_or("", |offer| offer.descriptor.resource_label.as_str())
    };
    let named: Vec<String> = std::iter::once(resource)
        .chain(members.iter().map(|(_, member)| *member))
        .map(|resource| format!("{resource} \"{}\"", label(resource)))
        .collect();
    let _ = writeln!(
        s,
        "In {app}: {} {} resources — {}; each takes, named as <resource>#operation:",
        named.len(),
        head.descriptor.resource_kind,
        named.join(", ")
    );
    for offer in offers {
        s.push_str(&summary_line(catalogue, offer).replacen(resource, "<resource>", 1));
    }
}

/// Resources of one application and kind with the same operations in the
/// same order, at least [`MOST_LISTED_ALIKE`] of them, by the first of each
/// group: the others. Each clip a task adds publishes the same operations
/// again; listed one by one they would fill the cap with one kind. A
/// resource with an open is never grouped: it is listed saying what it
/// opens.
fn alike_groups(catalogue: &Catalogue) -> BTreeMap<ResourceKey<'_>, Vec<ResourceKey<'_>>> {
    let opening: BTreeSet<ResourceKey<'_>> = catalogue
        .offers
        .iter()
        .filter(|offer| opened_app(offer).is_some())
        .map(resource_key)
        .collect();
    let mut resources: Vec<(ResourceKey<'_>, &str, Vec<&str>)> = Vec::new();
    let mut at: BTreeMap<ResourceKey<'_>, usize> = BTreeMap::new();
    for offer in &catalogue.offers {
        let key = resource_key(offer);
        if opening.contains(&key) {
            continue;
        }
        let action = offer.address().action.as_str();
        if let Some((_, _, actions)) = at.get(&key).and_then(|index| resources.get_mut(*index)) {
            actions.push(action);
        } else {
            at.insert(key, resources.len());
            resources.push((key, offer.descriptor.resource_kind.as_str(), vec![action]));
        }
    }
    let mut alike: BTreeMap<(&str, &str, &[&str]), Vec<ResourceKey<'_>>> = BTreeMap::new();
    for (key, kind, actions) in &resources {
        alike
            .entry((key.0, *kind, actions.as_slice()))
            .or_default()
            .push(*key);
    }
    alike
        .into_values()
        .filter(|members| members.len() >= MOST_LISTED_ALIKE)
        .filter_map(|mut members| {
            let rest = members.split_off(1);
            members.pop().map(|head| (head, rest))
        })
        .collect()
}

/// The offers of every resource `named` names that `chosen` does not hold
/// yet, opens aside: listed on top of the applications' shares rather than
/// out of them, since a re-plan's text holds what its reads answered, which
/// may name many resources, and taking them first would leave no room for
/// an application it names none of (the channel to reply in). A member of
/// an alike group pins its head, which stands for all of it.
fn named_offers<'c>(
    catalogue: &'c Catalogue,
    named: &Naming,
    head_of: &BTreeMap<ResourceKey<'c>, ResourceKey<'c>>,
    chosen: &[&'c Offer],
) -> Vec<&'c Offer> {
    let mut asked: BTreeSet<ResourceKey<'c>> = BTreeSet::new();
    let mut pinned: BTreeSet<ResourceKey<'c>> = BTreeSet::new();
    for offer in &catalogue.offers {
        let key = resource_key(offer);
        if asked.insert(key)
            && (named.names_resource(key.1) || named.names_label(&offer.descriptor.resource_label))
        {
            pinned.insert(head_of.get(&key).copied().unwrap_or(key));
        }
    }
    let held: BTreeSet<*const Offer> = chosen
        .iter()
        .map(|offer| std::ptr::from_ref(*offer))
        .collect();
    catalogue
        .offers
        .iter()
        .filter(|offer| {
            pinned.contains(&resource_key(offer))
                && opened_app(offer).is_none()
                && !held.contains(&std::ptr::from_ref(*offer))
        })
        .collect()
}

/// `chosen` with, after each offer of an alike group's head, the same
/// operation of every other resource of the group.
fn with_alike<'c>(
    catalogue: &'c Catalogue,
    head_of: &BTreeMap<ResourceKey<'c>, ResourceKey<'c>>,
    chosen: Vec<&'c Offer>,
) -> Vec<&'c Offer> {
    if head_of.is_empty() {
        return chosen;
    }
    let mut members: BTreeMap<(ResourceKey<'c>, &str), Vec<&'c Offer>> = BTreeMap::new();
    for offer in &catalogue.offers {
        if let Some(head) = head_of.get(&resource_key(offer)) {
            members
                .entry((*head, offer.address().action.as_str()))
                .or_default()
                .push(offer);
        }
    }
    let mut shown = Vec::with_capacity(chosen.len());
    for offer in chosen {
        shown.push(offer);
        if let Some(rest) = members.get(&(resource_key(offer), offer.address().action.as_str())) {
            shown.extend(rest.iter().copied());
        }
    }
    shown
}

/// The offers [`catalogue_summary`] shows, at most `most`: the running
/// applications' own operations take the first half, so the opens of
/// everything installed (a desktop lists every host application too) cannot
/// crowd them out; the opens follow, Tilo's applications before the host's,
/// whose windows publish nothing once open; what is left goes to more of
/// the running applications.
fn summarised<'a>(
    catalogue: &'a Catalogue,
    per_app: &BTreeMap<&str, Vec<&'a Offer>>,
    most: usize,
) -> Vec<&'a Offer> {
    fn fill<'a>(
        per_app: &BTreeMap<&str, Vec<&'a Offer>>,
        shown: &mut Vec<&'a Offer>,
        depth: &mut usize,
        until: usize,
    ) {
        while shown.len() < until {
            let mut any = false;
            for offers in per_app.values() {
                if let Some(offer) = offers.get(*depth) {
                    any = true;
                    if opened_app(offer).is_none() && shown.len() < until {
                        shown.push(offer);
                    }
                }
            }
            if !any {
                break;
            }
            *depth += 1;
        }
    }
    let mut shown: Vec<&'a Offer> = Vec::new();
    let mut depth = 0;
    fill(per_app, &mut shown, &mut depth, most.div_ceil(2));
    let mut opens: Vec<&Offer> = catalogue
        .offers
        .iter()
        .filter(|offer| opened_app(offer).is_some())
        .collect();
    opens
        .sort_by_key(|offer| opened_app(offer).is_some_and(|app| app.starts_with(HOST_APP_PREFIX)));
    let room = most.saturating_sub(shown.len());
    shown.extend(opens.into_iter().take(room));
    fill(per_app, &mut shown, &mut depth, most);
    shown
}

#[cfg(test)]
mod tests {
    use tilo_actions::InputSpec;

    use super::super::tests::offer;
    use super::*;

    /// A timeline whose project publishes a few operations, its export
    /// last, and whose `clips` clips each publish the same twenty.
    fn timeline(clips: usize) -> Catalogue {
        let mut offers = Vec::new();
        for action in ["place-clip", "add-title", "read-timeline", "render"] {
            offers.push(offer(
                "video-editor",
                "video-editor.project",
                "video-editor.project",
                action,
                action,
                InputSpec::None,
            ));
        }
        for n in 0..clips {
            let clip =
                format!("video-editor.project/clips/0000c11{n:x}-0000-4000-8000-000000000000");
            for op in 0..20 {
                let mut offer = offer(
                    "video-editor",
                    &clip,
                    "video-editor.clip",
                    &format!("set-op-{op}"),
                    "Set",
                    InputSpec::None,
                );
                offer.descriptor.resource_label = format!("shot-{n}.mp4");
                offers.push(offer);
            }
        }
        Catalogue {
            held: vec!["video-editor"],
            offers,
            ..Catalogue::default()
        }
    }

    /// Clips that publish the same operations share one line, counted once
    /// against the cap, so the project's own operations (its render) are
    /// still listed; every clip's operations may be named.
    #[test]
    fn look_alike_resources_share_a_line_and_leave_room_for_the_rest() {
        let catalogue = timeline(12);
        let named = Naming::in_text("cut the promo");
        let mut revealed = Revealed::new();
        let summary = catalogue_summary_with(&catalogue, &mut revealed, 30, Some(&named));
        assert!(
            summary.contains(
                "In video-editor: 12 video-editor.clip resources — \
                 video-editor.project/clips/0000c110-0000-4000-8000-000000000000 \"shot-0.mp4\""
            ),
            "{summary}"
        );
        assert!(
            summary.contains("  <resource>#set-op-19 — Set"),
            "{summary}"
        );
        assert!(
            summary.contains("video-editor.project#render"),
            "the project's render is not crowded out: {summary}"
        );
        assert!(
            revealed.contains(
                "video-editor.project/clips/0000c11b-0000-4000-8000-000000000000#set-op-19"
            )
        );
        assert_eq!(
            summary.matches("#set-op-3 ").count(),
            1,
            "one line per operation for all the clips: {summary}"
        );
        assert!(
            !summary.contains("more; ask"),
            "nothing left out: {summary}"
        );
        // Without naming (the task loop), the cap applies as before.
        let mut plain = Revealed::new();
        let summary = catalogue_summary(&catalogue, &mut plain, 30);
        assert!(
            !summary.contains("12 video-editor.clip resources"),
            "{summary}"
        );
        assert!(
            !plain.contains(
                "video-editor.project/clips/0000c11b-0000-4000-8000-000000000000#set-op-19"
            )
        );
        // Fewer than five alike are each listed as themselves.
        let summary = catalogue_summary_with(&timeline(4), &mut Revealed::new(), 200, Some(&named));
        assert!(!summary.contains("resources —"), "{summary}");
        assert_eq!(summary.matches("#set-op-3 ").count(), 4, "{summary}");
    }

    /// A clip an earlier step returned the id of is listed whatever the
    /// cap, and so is one the request names by its label; others past the
    /// cap are not.
    #[test]
    fn a_resource_named_by_its_id_or_label_is_listed_past_the_cap() {
        let mut catalogue = timeline(3);
        for offer in &mut catalogue.offers {
            // Three clips that are not alike: each its own operations.
            if let Some(n) = offer.descriptor.resource_label.strip_prefix("shot-") {
                let n = n.trim_end_matches(".mp4").to_owned();
                offer.descriptor.address.action =
                    format!("{}-{n}", offer.descriptor.address.action);
            }
        }
        let named = Naming::in_text(
            "Fade out SHOT-1.MP4 and the clip the step added: \
             {\"clip\":\"0000C112-0000-4000-8000-000000000000\"}",
        );
        let mut revealed = Revealed::new();
        let summary = catalogue_summary_with(&catalogue, &mut revealed, 10, Some(&named));
        assert!(
            revealed.contains(
                "video-editor.project/clips/0000c112-0000-4000-8000-000000000000#set-op-19-2"
            ),
            "named by its id: {summary}"
        );
        assert!(
            revealed.contains(
                "video-editor.project/clips/0000c111-0000-4000-8000-000000000000#set-op-19-1"
            ),
            "named by its label: {summary}"
        );
        assert!(
            !revealed.contains(
                "video-editor.project/clips/0000c110-0000-4000-8000-000000000000#set-op-19-0"
            ),
            "{summary}"
        );
        // A short id is no name: any number in the text could match it.
        assert!(!Naming::in_text("row 12 of 3").names_resource("sheets.workbook/rows/12"));
        assert!(!Naming::in_text("the sheets workbook").names_resource("sheets.workbook"));
    }

    /// What a re-plan's reads answered names every message, more operations
    /// than the cap holds. Those are listed on top of each application's
    /// share, so the channel no text names still offers its `post`; a
    /// channel named without Chat's leading `#` is named all the same.
    #[test]
    fn named_resources_past_the_cap_leave_other_applications_their_share() {
        let mut offers = Vec::new();
        for (channel, label) in [("ops", "#ops-room"), ("wa-lena", "#WhatsApp: Lena")] {
            for action in ["share-resource", "submit-feedback", "rename", "post"] {
                let mut offer = offer(
                    "chat",
                    &format!("chat.workspace/channels/{channel}"),
                    "chat.channel",
                    action,
                    action,
                    InputSpec::None,
                );
                offer.descriptor.resource_label = label.into();
                offers.push(offer);
            }
        }
        let mut ids = Vec::new();
        for n in 0..12 {
            let id = format!("0000a11{n:x}-0000-4000-8000-000000000000");
            for op in 0..10 {
                offers.push(offer(
                    "mail",
                    &format!("mail.mailbox/messages/{id}"),
                    "mail.message",
                    &format!("op-{op}-{n}"),
                    "Op",
                    InputSpec::None,
                ));
            }
            ids.push(id);
        }
        let catalogue = Catalogue {
            held: vec!["chat", "mail"],
            offers,
            ..Catalogue::default()
        };
        // Every message but the first is named by what the reads answered.
        let named = Naming::in_text(&format!(
            "reply in the WhatsApp: Lena channel; mail read answered {}",
            ids[1..].join(", ")
        ));
        let mut revealed = Revealed::new();
        let summary = catalogue_summary_with(&catalogue, &mut revealed, 8, Some(&named));
        assert!(
            revealed.contains("chat.workspace/channels/ops#post"),
            "chat keeps its share of the cap: {summary}"
        );
        assert!(
            revealed.contains("chat.workspace/channels/wa-lena#post"),
            "the channel named without its `#` is listed whole: {summary}"
        );
        assert!(
            revealed.contains("mail.mailbox/messages/0000a11b-0000-4000-8000-000000000000#op-9-11"),
            "every named message is listed past the cap: {summary}"
        );
        assert!(
            !revealed.contains("mail.mailbox/messages/0000a110-0000-4000-8000-000000000000#op-9-0"),
            "what nothing names stays within the cap: {summary}"
        );
    }
}
