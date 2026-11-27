//! Who may call a verb. Checked inside the verb, so every replica refuses a
//! change by the wrong kind of caller on replay, not only the host that
//! admitted it (bots.md §6).

use ui::tilo_resource::{ActionCx, MemberId, Refusal};

/// The caller is a bot.
///
/// # Errors
/// Anyone else.
pub fn bot(cx: &ActionCx) -> Result<(), Refusal> {
    if cx.invoked_by().is_some_and(|by| by.is_bot()) {
        Ok(())
    } else {
        Err(Refusal::new("only the bridge bot does this"))
    }
}

/// The caller is a person acting for themselves; returns their id.
pub fn person(cx: &ActionCx) -> Result<String, Refusal> {
    let by_person = cx.invoked_by().is_none_or(|by| by.is_person());
    match cx.author().and_then(MemberId::person_id) {
        Some(person) if by_person => Ok(person.to_owned()),
        _ => Err(Refusal::new("only a person does this, for themselves")),
    }
}

#[cfg(test)]
mod tests {
    use ui::tilo_actions::executor::with_invoking_context;
    use ui::tilo_actions::remote::Caller;
    use ui::tilo_actions::{ActionContext, ActionInvoker};
    use ui::tilo_resource::ActionCx;
    use ui::tilo_resource::model::Doc;

    /// Who [`super::person`] says is binding their own account, for a verb `caller` invokes.
    fn person_of(caller: &Caller) -> Option<String> {
        let context = ActionContext {
            invoker: Some(ActionInvoker::admitted(caller.acting())),
            ..ActionContext::default()
        };
        with_invoking_context(Some(context), || {
            super::person(&ActionCx::for_apply(&Doc::new(), b"", 0)).ok()
        })
    }

    /// Binding a Discord account is its person's own act: a Shortcut's unattended run, saved by
    /// them or by their agent, binds nobody's.
    #[test]
    fn a_shortcuts_run_binds_no_account() {
        let ada = "ad".repeat(32);
        let own = Caller::local_person(ada.as_str());
        assert_eq!(person_of(&own).as_deref(), Some(ada.as_str()));
        let agent = Caller::local_agent("scout", ada.as_str());
        assert_eq!(person_of(&agent), None, "an agent binds nothing");
        for run in [
            own.running_shortcut("reminder"),
            agent.running_shortcut("reminder"),
        ] {
            assert_eq!(person_of(&run), None, "{:?}", run.acting());
        }
    }
}
