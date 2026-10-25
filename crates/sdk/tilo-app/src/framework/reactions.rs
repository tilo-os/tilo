//! The reactions to a window's record, in the window's bar.
//!
//! A record that holds a [`Reactions`](tilo_resource::Reactions) field
//! takes reactions to itself, and a framework window showing it offers them
//! where the window's other system controls are: at the end of its bar, a
//! pill per emoji and an add button that opens the shared emoji picker
//! ([`ui_core::window_toolbar::show_reactions`]). No app wires it; holding
//! the field is the whole opt-in.
//!
//! The bar shows them while the record is shared with someone else, or
//! already has reactions: alone in a record of one's own, nobody would see
//! a reaction. A past version shows none. Someone the room does not let
//! react (a viewer) sees the reactions there are, and no add button; a
//! commenter may react, as they may comment.
//!
//! A press on a pill, or a pick in the picker, is the person's reaction
//! with that emoji, or takes it back where they had it
//! ([`Reactions::toggle`](tilo_resource::Reactions::toggle)): the field
//! edit goes through the window's dispatcher, as the person.

use tilo_resource::Segment;
use tilo_resource::reactions::record_reactions;
use ui_core::presence::ParticipantKind;
use ui_core::window_toolbar::{ToolbarReaction, ToolbarReactions, show_reactions};

use super::People;
use super::dispatch::DispatchCore;

/// What the window's bar shows of the reactions to the window's record.
#[derive(Default)]
pub(crate) struct ReactionsIndicator {
    /// Whether the bar shows any, so a window without them clears none it
    /// never showed.
    showing: bool,
}

/// What the indicator reads on a sync.
pub(crate) struct ReactionsInput<'a> {
    /// The record's own table ([`tilo_resource::Resource::RECORD_TABLE`]).
    pub(crate) table: Option<&'static tilo_resource::table::ModelTable>,
    /// The record as it stands now.
    pub(crate) state: &'a dyn std::any::Any,
    pub(crate) dispatch: &'a DispatchCore,
    pub(crate) people: &'a People,
    /// The window is private: nobody else ever sees it.
    pub(crate) private: bool,
}

impl ReactionsIndicator {
    /// On every sync of the window: run the reactions pressed in the bar,
    /// then show the record's reactions as they stand.
    pub(crate) fn sync(&mut self, input: &ReactionsInput<'_>) {
        let presses = ui_core::window_toolbar::take_reaction_presses();
        let Some((key, reactions)) = input
            .table
            .and_then(|table| record_reactions(table, input.state))
        else {
            self.show(None);
            return;
        };
        let me = input.dispatch.me();
        if let Some(me) = me {
            for emoji in presses {
                input.dispatch.submit_reaction(
                    vec![Segment::Field(key.to_owned())],
                    reactions.toggle(&emoji, me),
                );
            }
        }
        let shown = shown(input, reactions, me);
        self.show(shown);
    }

    fn show(&mut self, reactions: Option<ToolbarReactions>) {
        if reactions.is_none() && !self.showing {
            return;
        }
        self.showing = reactions.is_some();
        show_reactions(reactions);
    }
}

/// What the bar shows of `reactions`, or `None` where it shows nothing.
fn shown(
    input: &ReactionsInput<'_>,
    reactions: &tilo_resource::Reactions,
    me: Option<&str>,
) -> Option<ToolbarReactions> {
    if input.dispatch.is_past() {
        return None;
    }
    let can_react = me.is_some() && input.dispatch.may_react();
    let shared = !input.private
        && input
            .people
            .members()
            .iter()
            .any(|member| !member.is_self && member.kind == ParticipantKind::Person);
    let groups = reactions.groups();
    if groups.is_empty() && !(shared && can_react) {
        return None;
    }
    let pills = groups
        .iter()
        .map(|group| ToolbarReaction {
            emoji: group.emoji.clone(),
            count: u32::try_from(group.count()).unwrap_or(u32::MAX),
            mine: me.is_some_and(|me| group.includes(me)),
            names: group
                .reactors
                .iter()
                .filter(|reactor| reactor.persona.is_some() || me.is_none_or(|me| reactor.by != me))
                .take(ui_core::window_toolbar::MAX_REACTOR_NAMES)
                .map(|reactor| {
                    reactor
                        .name
                        .clone()
                        .unwrap_or_else(|| input.people.name(&reactor.by))
                })
                .collect(),
        })
        .collect();
    Some(ToolbarReactions { pills, can_react })
}
