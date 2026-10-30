//! What the window does beside its record: the selection History shows and
//! the morning sources What's new reads, each an effect a row's button, the
//! palette, an agent or a script invokes alike.

use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;

use crate::{Nav, Provider};

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "activity.effects")]
pub enum ActivityEffect {
    /// Select every change from one change through another in History.
    #[effect(view, label = "Select through here")]
    SelectThrough {
        #[param("Id of the change the selection starts at")]
        from: String,
        #[param("Id of the change the selection ends at")]
        to: String,
    },
    /// Read a morning source again.
    #[effect(label = "Refresh source")]
    RefreshSource {
        #[param("Key of the morning source")]
        key: String,
    },
    /// Stop reading a morning source.
    #[effect(label = "Remove source")]
    RemoveSource {
        #[param("Key of the morning source")]
        key: String,
    },
}

/// Answer `effect` on the window `nav` shows, asking the workspace's history
/// through `provider`.
pub(crate) fn answer(effect: ActivityEffect, nav: &Nav, provider: &Provider) {
    match effect {
        ActivityEffect::SelectThrough { from, to } => {
            nav.select(Some(wire::Range { from, to }));
        }
        ActivityEffect::RefreshSource { key } => {
            source(provider, key, wire::SourceCommand::Refresh);
        }
        ActivityEffect::RemoveSource { key } => {
            source(provider, key, wire::SourceCommand::Remove);
        }
    }
}

fn source(provider: &Provider, key: String, command: wire::SourceCommand) {
    provider.ask(wire::SOURCE, &wire::SourceRequest { key, command });
}
