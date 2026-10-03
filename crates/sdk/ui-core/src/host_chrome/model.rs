//! One command model for every host chrome.
//!
//! A command is only a view of an action that already exists: its binding
//! addresses a resource the host serves, its enabled state is that action's
//! availability, and invoking it opens the same door the palette uses.

use crate::time::SystemTime;

use fluent::FluentArgs;
use tilo_actions::{ActionBinding, ActionSpec, ControlInvocation, OperationKind, Resource};

use super::edit::EDIT_RESOURCE;
use crate::i18n::I18nManager;
use crate::platform::{HostShortcut, PlatformConventions, Shortcut};
use crate::search::request::SEARCH_RESOURCE;
use crate::standalone_history_host::{HISTORY_RESOURCE, encode_entry};
use crate::widget::HistoryEntryId;

const HISTORY_SESSION_KIND: &str = "tilo.host.history.session";
/// How many recent changes a History menu lists.
pub(crate) const RECENT_CHANGES: usize = 8;

/// Where a command sits. Hosts map groups onto their own menus and
/// separate adjacent groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandGroup {
    Undo,
    Search,
    /// The app's own document and effect actions.
    AppActions,
    /// The app's own view actions.
    AppViews,
    /// Show or hide History, and compare selected changes.
    History,
    /// Recent changes, newest first.
    RecentChanges,
}

#[derive(Clone)]
pub(crate) struct HostCommand {
    pub binding: ActionBinding<()>,
    pub label: String,
    pub shortcut: Option<Shortcut>,
    pub enabled: bool,
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only the macOS menu bar places commands so far")
    )]
    pub group: CommandGroup,
}

impl HostCommand {
    fn new(resource: &Resource, action: &ActionSpec, group: CommandGroup) -> Option<Self> {
        Some(Self {
            binding: ActionBinding::unit(resource, action).ok()?,
            label: action.label.clone(),
            shortcut: None,
            enabled: action.contract.availability.is_available(),
            group,
        })
    }

    fn with_shortcut(mut self, service: HostShortcut, conventions: PlatformConventions) -> Self {
        self.shortcut = Some(service.shortcut(conventions));
        self
    }

    fn with_label(mut self, key: &str) -> Self {
        self.label = I18nManager::lookup_plain(key);
        self
    }

    /// What activating the command asks the host to do.
    pub(crate) fn invocation(&self) -> Option<ControlInvocation> {
        self.binding.invoke(()).ok()
    }
}

/// A change a History menu lists, from the same rows the History pane shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecentChange {
    pub entry: HistoryEntryId,
    pub author: String,
    /// The action's own label ("Rename") where it has one, else its summary.
    pub label: String,
    pub at: SystemTime,
}

impl RecentChange {
    pub(crate) fn from_entry(entry: &crate::widget::HistoryEntry) -> Self {
        Self {
            entry: entry.id,
            author: entry.author.name.clone(),
            label: entry.context.as_ref().map_or_else(
                || entry.summary.clone(),
                |context| context.action_label.clone(),
            ),
            at: entry.timestamp,
        }
    }

    fn title(&self, now: SystemTime) -> String {
        let mut args = FluentArgs::new();
        args.set("author", self.author.clone());
        args.set("change", self.label.clone());
        args.set("when", crate::widget::format_relative_time(self.at, now));
        I18nManager::lookup("host-history-recent", Some(&args))
    }
}

/// The commands a host chrome presents, in display order. `resources` are
/// the host's own (History, Search, Undo) followed by the app's declared
/// ones; only nullary, person-facing app actions become commands.
pub(crate) fn host_commands(
    resources: &[Resource],
    recent: &[RecentChange],
    conventions: PlatformConventions,
    now: SystemTime,
) -> Vec<HostCommand> {
    let find = |id: &str| resources.iter().find(|resource| resource.id.as_str() == id);
    let mut commands = Vec::new();

    if let Some(edit) = find(EDIT_RESOURCE) {
        for (id, service) in [("undo", HostShortcut::Undo), ("redo", HostShortcut::Redo)] {
            commands.extend(
                action(edit, id)
                    .and_then(|spec| HostCommand::new(edit, spec, CommandGroup::Undo))
                    .map(|command| command.with_shortcut(service, conventions)),
            );
        }
    }
    if let Some(search) = find(SEARCH_RESOURCE) {
        commands.extend(
            action(search, "open")
                .and_then(|spec| HostCommand::new(search, spec, CommandGroup::Search))
                .map(|command| {
                    command
                        .with_label("host-search")
                        .with_shortcut(HostShortcut::Search, conventions)
                }),
        );
    }

    for resource in resources
        .iter()
        .filter(|resource| is_app_resource(resource))
    {
        for spec in &resource.actions {
            if spec.is_programmatic() {
                continue;
            }
            let group = if spec.kind == OperationKind::View {
                CommandGroup::AppViews
            } else {
                CommandGroup::AppActions
            };
            commands.extend(HostCommand::new(resource, spec, group));
        }
    }

    let session = resources
        .iter()
        .find(|resource| resource.kind == HISTORY_SESSION_KIND);
    let history = find(HISTORY_RESOURCE);
    if let Some(history) = history {
        let toggle = match action(history, "hide") {
            Some(hide) => HostCommand::new(history, hide, CommandGroup::History),
            None => action(history, "open")
                .and_then(|open| HostCommand::new(history, open, CommandGroup::History))
                .map(|command| command.with_label("host-history-show")),
        };
        commands.extend(
            toggle.map(|command| command.with_shortcut(HostShortcut::History, conventions)),
        );
    }
    if let Some(session) = session {
        commands.extend(
            action(session, "compare")
                .and_then(|spec| HostCommand::new(session, spec, CommandGroup::History))
                .map(|command| command.with_label("host-history-compare")),
        );
    }
    if let Some(history) = history
        && let Some(spec) = action(history, "open_at")
    {
        let enabled =
            action(history, "open").is_some_and(|open| open.contract.availability.is_available());
        for change in recent.iter().take(RECENT_CHANGES) {
            let mut arguments = serde_json::Map::new();
            arguments.insert("entry".into(), encode_entry(change.entry).into());
            let Ok(binding) = ActionBinding::static_json(history, spec, arguments) else {
                continue;
            };
            commands.push(HostCommand {
                binding,
                label: change.title(now),
                shortcut: None,
                enabled,
                group: CommandGroup::RecentChanges,
            });
        }
    }
    commands
}

fn action<'a>(resource: &'a Resource, id: &str) -> Option<&'a ActionSpec> {
    resource.actions.iter().find(|action| action.id == id)
}

/// The host's own resources are presented by their own commands above; the
/// broker-facing `tilo.history` door is the same History.
fn is_app_resource(resource: &Resource) -> bool {
    let id = resource.id.as_str();
    !(id.starts_with("tilo.host") || id == crate::app::HISTORY_ACTION_RESOURCE)
}
