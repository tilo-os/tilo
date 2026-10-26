//! What a task is planned with ([`PlanContext`]): the records it names,
//! opened or made before the model is asked; what they hold now; what the
//! apps can do; and the room's records the planner may search.

use std::collections::BTreeMap;

use automerge::ReadDoc as _;
use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::DocumentRoute;

use super::unopened::Reading;
use super::{
    AgentParticipant, MOST_OPENED_BY_TITLE, MOST_REFERENCED_APPS, MOST_SNAPSHOT_CHARS,
    MOST_SNAPSHOT_READS, MOST_SNAPSHOTS, MOST_SNAPSHOTS_CHARS, SNAPSHOT_READS, actions,
};
use crate::apps::Seed;
use crate::planner::{PlanContext, observation_within, readable};
use crate::worker::ConfinedReplica;

impl AgentParticipant {
    /// The room's records the agent has not opened, those people have open
    /// first, each of `apps` named as its app names it (a document's first
    /// heading) when it is among those offered to open, so a task can say
    /// which: read in a confined worker, started only when one changed
    /// since it was read.
    pub(super) fn known_titled(&mut self, apps: &[String]) -> Vec<(DocumentRoute, Option<String>)> {
        let mut offered: BTreeMap<&str, usize> = BTreeMap::new();
        let mut titles = crate::worker::ConfinedTitles::default();
        self.unopened
            .people_first()
            .into_iter()
            .map(|route| {
                let Some(app) = apps.iter().find(|app| **app == route.app_id) else {
                    return (route, None);
                };
                let count = offered.entry(app.as_str()).or_default();
                if *count >= crate::model::KNOWN_OFFERED {
                    return (route, None);
                }
                *count = count.saturating_add(1);
                let title = self
                    .unopened
                    .title(&route, |doc| titles.title(&route.app_id, doc));
                (route, title)
            })
            .collect()
    }

    /// What `task` is planned with. Before the model is asked, the records
    /// of the room the task names by their titles are opened; an app it is
    /// about of which the room has no record yet is previewed; and what the
    /// open records it is about hold is read. `request` is the task as it
    /// was given when `task` is the rest of it.
    pub(super) fn plan_context(&mut self, task: &str, request: Option<String>) -> PlanContext {
        let apps: Vec<String> = self
            .catalogue
            .iter()
            .map(|definition| definition.app_id.to_owned())
            .filter(|app| self.identity.delegation.permits_app(app))
            .collect();
        let mut known = self.known_titled(&apps);
        // A follow-up is about the records the request named too: its own
        // text ("check the work", "now export it") names none of them.
        let mut naming = naming_text(task, request.as_deref());
        // An item of a coordinated task is about the records its request
        // names, as one agent planning the whole would be.
        if let Some(whole) = self.item_request() {
            naming = naming_text(&naming, Some(&whole));
        }
        let mut about = self.open_named(&naming, &apps, &known);
        about.extend(self.open_new(&naming, &apps, &known));
        known.retain(|(route, _)| !self.apps.contains_key(route));
        let mut open: Vec<(DocumentRoute, Vec<ActionDescriptor>)> = self
            .apps
            .iter()
            .map(|(route, app)| (route.clone(), app.survey()))
            .collect();
        // The records the task names, by title or by app, are what it is
        // about, and only theirs are offered: a record open from an earlier
        // task (the note, beside the page under review) would cost its
        // whole list on every plan. A task that names none is offered all.
        let touched = self.touched();
        let named: Vec<DocumentRoute> = open
            .iter()
            .map(|(route, _)| route)
            .filter(|route| {
                about.contains(route)
                    || touched.contains(route)
                    || self.task_read.contains(route)
                    || crate::model::task_names_app(&naming, &route.app_id)
            })
            .cloned()
            .collect();
        let mut about = if named.is_empty() {
            open.iter().map(|(route, _)| route.clone()).collect()
        } else {
            open.retain(|(route, _)| named.contains(route));
            named
        };
        let others = self.set_aside_others_documents(&naming, &touched, &mut open, &mut known);
        about.retain(|route| !others.contains(route));
        // Only an app of which the room has no record yet: where it has
        // one, a task about the app may well mean it ("add a sheet"), and
        // is offered to open it rather than to start another. Others'
        // written documents do not count: the task writes its own.
        let previews: Vec<(String, Vec<ActionDescriptor>)> = apps
            .iter()
            .filter(|app| {
                !open.iter().any(|(route, _)| route.app_id == **app)
                    && !known
                        .iter()
                        .any(|(route, _)| route.app_id == **app && !others.contains(route))
                    && (apps.len() <= 2 || crate::model::task_names_app(&naming, app))
            })
            .filter_map(|app| Some((app.clone(), self.preview(app)?)))
            .collect();
        let mut snapshots = self.snapshots(&about, &open);
        snapshots.extend(content_facts(task, &self.content));
        // What the apps can do, the same whatever the room holds: every
        // granted app's, or for an agent granted many, those in play.
        let referenced: Vec<String> = apps
            .iter()
            .filter(|app| {
                apps.len() <= MOST_REFERENCED_APPS
                    || open.iter().any(|(route, _)| route.app_id == **app)
                    || previews.iter().any(|(previewed, _)| previewed == *app)
                    || crate::model::task_names_app(&naming, app)
            })
            .cloned()
            .collect();
        let reference = self.reference(referenced);
        let (produced, observed, (pictures, rendered), obligations) = self
            .running
            .as_ref()
            .map(|running| {
                (
                    running.produced.clone(),
                    running.observations(),
                    running.pictures(request.is_some()),
                    request.as_ref().map(|_| running.checklist.shown()),
                )
            })
            .unwrap_or_default();
        let searchable = self.searchable(&apps, &open);
        PlanContext {
            open,
            known,
            apps,
            produced,
            observed,
            previews,
            snapshots,
            request,
            reference,
            room: self.room.clone(),
            searchable,
            may_plan_nothing: false,
            pictures,
            // Filled where a plan of the running task is asked for.
            read_pictures: Vec::new(),
            rendered,
            room_pictures: self.room_pictures == crate::vision::RoomPictures::Shown,
            new_documents: self.new_documents(),
            retries: self.task_retries.clone(),
            obligations: obligations.unwrap_or_default(),
            role: crate::planner::Role::Whole,
        }
    }

    /// The apps the running task is offered a new document of
    /// ([`PlanContext::new_documents`]).
    fn new_documents(&self) -> Vec<String> {
        self.running
            .as_ref()
            .map(|running| running.new_documents.clone())
            .unwrap_or_default()
    }

    /// The room's records of `apps` for the planner's search
    /// ([`PlanContext::searchable`]): those the agent has open (`listed`
    /// are those whose operations the plan lists), then the others, those
    /// people have open first, then the latest changed, at most
    /// [`crate::search::MOST_SEARCHED`]. A record's title and text are read
    /// once per change of it.
    pub(super) fn searchable(
        &mut self,
        apps: &[String],
        listed: &[(DocumentRoute, Vec<ActionDescriptor>)],
    ) -> Vec<crate::search::SearchDoc> {
        let mut docs = Vec::new();
        for (route, replica) in &self.apps {
            if !apps.contains(&route.app_id) {
                continue;
            }
            if let Some(text) = self.unopened.open_text(route, replica, Reading::Plain) {
                docs.push(crate::search::SearchDoc {
                    route: route.clone(),
                    title: None,
                    text,
                    changed: None,
                    open: listed.iter().any(|(open, _)| open == route),
                });
            }
        }
        let mut titles = crate::worker::ConfinedTitles::default();
        for route in self.unopened.people_first() {
            if docs.len() >= crate::search::MOST_SEARCHED {
                break;
            }
            if !apps.contains(&route.app_id) {
                continue;
            }
            let title = self
                .unopened
                .title(&route, |doc| titles.title(&route.app_id, doc));
            let Some(text) = self.unopened.text(&route, Reading::Plain) else {
                continue;
            };
            let changed = self.unopened.changed(&route);
            docs.push(crate::search::SearchDoc {
                route,
                title,
                text,
                changed,
                open: false,
            });
        }
        let kept: std::collections::HashSet<&DocumentRoute> =
            docs.iter().map(|doc| &doc.route).collect();
        self.unopened.keep_texts(|route| kept.contains(route));
        docs
    }

    /// Make a new record of each of `apps` the task names that the room has
    /// none of, before it is planned: its channel's keys are asked for and
    /// granted while the model plans, rather than after, when the plan's
    /// first step would make it. Returns the records made.
    pub(super) fn open_new(
        &mut self,
        task: &str,
        apps: &[String],
        known: &[(DocumentRoute, Option<String>)],
    ) -> Vec<DocumentRoute> {
        let mut made = Vec::new();
        for app in apps {
            if !crate::model::task_names_app_itself(task, app)
                || self.apps.keys().any(|route| route.app_id == *app)
                || known.iter().any(|(route, _)| route.app_id == *app)
            {
                continue;
            }
            match self.open(app, None) {
                Ok(route) => {
                    // A new record publishes nothing until its first edit:
                    // ask for its channel's keys now.
                    if let Err(error) = self.peer.open_route(route.clone()) {
                        tracing::warn!(%error, "agent: keys for a new record were not asked for");
                    }
                    // The task's to fill: an import naming no record goes
                    // here rather than into a second, new one.
                    self.task_made.push(route.clone());
                    made.push(route);
                }
                Err(error) => tracing::warn!(%error, app, "agent: a new record did not open"),
            }
        }
        if !made.is_empty() {
            self.publish_changes();
        }
        made
    }

    /// Open the room's records of `apps` whose titles `task` names (of
    /// `known`, each with its title) before it is planned. Opening is
    /// local, and an open record publishes its operations and can be read,
    /// so the task is planned in one go rather than opened, then planned
    /// again. Returns the records opened.
    pub(super) fn open_named(
        &mut self,
        task: &str,
        apps: &[String],
        known: &[(DocumentRoute, Option<String>)],
    ) -> Vec<DocumentRoute> {
        let lower = task.to_lowercase();
        let mut named: Vec<&DocumentRoute> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (route, title) in known {
            let Some(title) = title.as_deref().map(str::trim) else {
                continue;
            };
            let title = title.to_lowercase();
            if named.len() >= MOST_OPENED_BY_TITLE
                || title.chars().count() < 3
                || seen.contains(&title)
                || !apps.contains(&route.app_id)
                || !lower.contains(&title)
            {
                continue;
            }
            seen.push(title);
            named.push(route);
        }
        // A task about an app the room has one record of means that one,
        // titled or not (a spreadsheet has no title to name it by). A blank
        // record (one another task opened new and has not filled yet) is
        // not counted: it holds nothing the task could mean.
        for app in apps {
            if !crate::model::task_names_app(task, app)
                || named.iter().any(|route| route.app_id == *app)
                || self.apps.keys().any(|route| route.app_id == *app)
            {
                continue;
            }
            // Nor is another member's written document: the task, which
            // does not name it, is about a document of its own.
            let mut routes: Vec<DocumentRoute> = Vec::new();
            for (route, _) in known {
                if route.app_id == *app && !self.others_document(route, task) {
                    routes.push(route.clone());
                }
            }
            if let Some(only) = self.only_written(routes)
                && let Some((route, _)) = known.iter().find(|(route, _)| *route == only)
            {
                named.push(route);
            }
        }
        let mut opened = Vec::new();
        for route in named {
            match self.open(&route.app_id, Some(&route.record)) {
                Ok(route) => opened.push(route),
                Err(error) => {
                    tracing::warn!(%error, record = %route.record, "agent: a named record did not open");
                }
            }
        }
        opened
    }

    /// What the records at `about` (of `open`, each with what it publishes)
    /// hold now: [`Self::snapshot`]s of the first [`MOST_SNAPSHOTS`], within
    /// [`MOST_SNAPSHOTS_CHARS`] together; on a check, the facts of what the
    /// task changed too ([`Self::check_facts`]).
    pub(super) fn snapshots(
        &mut self,
        about: &[DocumentRoute],
        open: &[(DocumentRoute, Vec<ActionDescriptor>)],
    ) -> Vec<String> {
        let mut snapshots = Vec::new();
        let mut left = MOST_SNAPSHOTS_CHARS;
        for route in about.iter().take(MOST_SNAPSHOTS) {
            let descriptors = open
                .iter()
                .find(|(open, _)| open == route)
                .map(|(_, descriptors)| descriptors.as_slice())
                .unwrap_or_default();
            for seen in self.snapshot(route, descriptors, left.min(MOST_SNAPSHOT_CHARS)) {
                left = left.saturating_sub(seen.chars().count());
                snapshots.push(seen);
            }
        }
        snapshots.extend(self.check_facts(open));
        snapshots
    }

    /// `apps`, sorted, each with what a new record of it publishes
    /// ([`PlanContext::reference`]).
    pub(super) fn reference(
        &mut self,
        mut apps: Vec<String>,
    ) -> Vec<(String, Vec<ActionDescriptor>)> {
        apps.sort();
        apps.into_iter()
            .filter_map(|app| {
                let descriptors = self.preview(&app)?;
                Some((app, descriptors))
            })
            .collect()
    }

    /// What a new record of `app` publishes, surveyed once in a record that
    /// is never published. `None` when the app could not be opened.
    pub(super) fn preview(&mut self, app: &str) -> Option<Vec<ActionDescriptor>> {
        if let Some(descriptors) = self.previews.get(app) {
            return Some(descriptors.clone());
        }
        let record = ConfinedReplica::open(
            &self.catalogue,
            app,
            &self.room,
            crate::control::agent_id(&self.identity.delegation),
            &crate::ReplicaActor::Agent,
            Seed::New,
        );
        let descriptors = match record {
            Ok(record) => {
                if let Some(changes) = record.read(|doc| doc.stats().num_changes) {
                    self.genesis_changes.insert(app.to_owned(), changes);
                }
                record.survey()
            }
            Err(error) => {
                tracing::warn!(%error, %app, "agent: an app could not be previewed");
                return None;
            }
        };
        if descriptors.is_empty() {
            return None;
        }
        self.previews.insert(app.to_owned(), descriptors.clone());
        Some(descriptors)
    }

    /// What the open record at `route` holds now, read with the first of
    /// [`SNAPSHOT_READS`] it publishes (of `descriptors`) on each resource
    /// that publishes it, summarised within `most` characters.
    pub(super) fn snapshot(
        &mut self,
        route: &DocumentRoute,
        descriptors: &[ActionDescriptor],
        most: usize,
    ) -> Vec<String> {
        let takes_nothing = |descriptor: &&ActionDescriptor| {
            readable(descriptor) && descriptor.params.iter().all(|param| !param.required)
        };
        let Some(action) = SNAPSHOT_READS.iter().find(|action| {
            descriptors
                .iter()
                .filter(takes_nothing)
                .any(|descriptor| descriptor.address.action == **action)
        }) else {
            return Vec::new();
        };
        let mut left = most;
        let mut found = Vec::new();
        for descriptor in descriptors
            .iter()
            .filter(takes_nothing)
            .filter(|descriptor| descriptor.address.action == *action)
            .take(MOST_SNAPSHOT_READS)
        {
            if left < 200 {
                break;
            }
            let output = match self.read_now(route, &descriptor.address, &serde_json::Map::new()) {
                Ok(output) => output,
                Err(error) => {
                    tracing::debug!(%error, "agent: a record could not be read for its plan");
                    continue;
                }
            };
            if let Some(seen) =
                observation_within(&route.app_id, &descriptor.address, &output, left)
            {
                left = left.saturating_sub(seen.chars().count());
                found.push(format!("(document {}) {seen}", route.record));
            }
        }
        found
    }

    /// Run the read at `address` of the open record at `route`: only an
    /// operation the record publishes now that only reads it
    /// ([`readable`]), never confirmed for anyone.
    pub(super) fn read_now(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, String> {
        let app = self
            .apps
            .get(route)
            .ok_or_else(|| "that document is not open".to_string())?;
        let descriptor = app
            .survey()
            .into_iter()
            .find(|descriptor| descriptor.address == *address)
            .ok_or_else(|| format!("{} is not published now", address.action))?;
        if !readable(&descriptor) {
            return Err(format!(
                "{} changes or makes something; only reads run while planning",
                descriptor.action_label
            ));
        }
        self.invoke_as(route, address, arguments, false)
            .map(Option::unwrap_or_default)
            .map_err(|refusal| match refusal {
                actions::Refusal::Door(error) => format!("{}: {error}", error.code()),
                actions::Refusal::Agent(reason) => reason,
            })
    }
}

/// What the task's `tilo.content:` addresses hold, as far as a plan needs
/// it without reading them: a picture's format and size, a video's length,
/// frame, rate, sound and weight.
pub(super) fn content_facts(task: &str, content: &tilo_content::ContentStore) -> Vec<String> {
    const PREFIX: &str = "tilo.content:";
    let mut facts = Vec::new();
    for (at, _) in task.match_indices(PREFIX) {
        let hash: String = task
            .get(at.saturating_add(PREFIX.len())..)
            .unwrap_or_default()
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        let fact = content.read(&hash).and_then(|bytes| {
            crate::vision::picture_size(&bytes)
                .map(|(format, width, height)| {
                    format!("{PREFIX}{hash} is a {format} picture {width}×{height} pixels.")
                })
                .or_else(|| video_facts(&bytes).map(|video| format!("{PREFIX}{hash} {video}")))
        });
        if let Some(fact) = fact
            && !facts.contains(&fact)
        {
            facts.push(fact);
        }
    }
    facts
}

/// What an MP4 or MOV file `bytes` holds, as a sentence (`is a video
/// of 8.50 s, 1280×720 at 30 fps, with sound, 412 kB.`): `None` for
/// anything else.
pub(super) fn video_facts(bytes: &[u8]) -> Option<String> {
    if bytes.get(4..8) != Some(b"ftyp".as_slice()) {
        return None;
    }
    let probed =
        video_editor::probe::probe_bytes(bytes, std::path::Path::new("content.mp4")).ok()?;
    if probed.kind != video_editor::model::MediaKind::Video {
        return None;
    }
    let sound = if probed.has_audio {
        "with sound"
    } else {
        "without sound"
    };
    Some(format!(
        "is a video of {:.2} s, {}×{} at {} fps, {sound}, {}.",
        probed.duration,
        probed.width,
        probed.height,
        (probed.fps * 100.0).round() / 100.0,
        ui::format::size(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    ))
}

/// The content `reader` reads, to show a planning model: at most one byte
/// more than [`crate::vision::MOST_SOURCE_BYTES`], so a video is never read
/// whole only to be refused as too large.
pub(super) fn picture_head(
    reader: tilo_content::ContentReader,
    hash: &str,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let most = u64::try_from(crate::vision::MOST_SOURCE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    reader
        .take(most)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("the content tilo.content:{hash} could not be read: {error}"))?;
    Ok(bytes)
}

/// The text a plan's records are chosen by: `task`, and the request it is
/// the rest of when there is one.
pub(super) fn naming_text(task: &str, request: Option<&str>) -> String {
    match request {
        Some(request) if request != task => format!("{request}\n{task}"),
        _ => task.to_owned(),
    }
}
