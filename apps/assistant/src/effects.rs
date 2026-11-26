//! What the window does beside its record: which place it shows, and handing the main agent
//! a job for a sub-agent. Each is an effect a control of the window, the palette, an agent or
//! a script invokes alike.

use ui::prelude::*;

/// The Assistant window's effects.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "assistant.effects")]
pub enum AssistantEffect {
    // Chat opens in another app and answers with the caller's grants.
    #[effect(kind = "read", job)]
    ViewMainChat,
    #[effect(view)]
    ViewSubAgent {
        #[param("Id of the sub-agent")]
        agent: String,
    },
    #[effect(view)]
    ViewSupervision,
    #[effect(kind = "outside")]
    #[effect(job)]
    StartSubAgent {
        #[param("The job, in the person's own words")]
        job: String,
    },
    #[effect(kind = "outside", job)]
    OpenModelSettings,
}
