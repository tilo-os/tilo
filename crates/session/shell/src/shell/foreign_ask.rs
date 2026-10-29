//! Asking the person, on a device of theirs, what a device of theirs hosts.
//!
//! An action the person's grants leave to them asks first. At a device that
//! is theirs this host asks them itself; a device that is *not* theirs — a
//! Tilo runtime on an operating system this person does not control — has
//! nobody to ask at its own screen, so the question travels to one of their
//! own devices, which puts it on the same "… wants to act for you" sheet.
//! The answer names that one invocation; nothing about a grant changes.

use tilo_actions::remote::{
    ActionHostError, ConfirmationQuestion, ConfirmationReply, Pending, RemoteActionMessage,
};

use crate::shell::network::ShellNetwork;

impl ShellNetwork {
    /// Put `question` to the person on one of their own devices, `room`
    /// being where the question's invocation runs, and wait for what they
    /// chose there.
    ///
    /// # Errors
    ///
    /// Why nobody can be asked: this device is not enrolled with one of the
    /// person's own devices that shares `room` and is online, or the
    /// question could not be handed to the transport. An answer that never
    /// comes is the caller's to time out, never a confirmation.
    pub(crate) fn ask_person(
        &self,
        question: &ConfirmationQuestion,
        room: crate::shell::state::RoomId,
    ) -> Result<Pending<Result<ConfirmationReply, ActionHostError>>, ActionHostError> {
        let unavailable = |reason: String| ActionHostError::Unavailable { reason };
        let local = self
            .personal_continuity_device
            .as_ref()
            .and_then(|device| crate::shell::network::decode_lower_hex_32(device.as_str()));
        let Some((endpoint, topic)) =
            self.enrolled_personal_devices(local)
                .into_iter()
                .find_map(|addr| {
                    self.online_enrolled_room(addr.id, Some(room))
                        .ok()
                        .map(|shared| (addr.id, tilo_network_actor::room_topic_id(shared)))
                })
        else {
            return Err(unavailable(
                "no device of theirs is reachable in this workspace".into(),
            ));
        };
        let correlation = self
            .next_foreign_ask
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let answered = self.remote_action_replies.register(
            endpoint,
            crate::shell::remote_action_link::ReplyKey::Confirmation(correlation),
        );
        if !self.send_remote_action(
            topic,
            endpoint,
            RemoteActionMessage::ConfirmationRequest {
                correlation,
                question: question.clone(),
            },
        ) {
            return Err(unavailable(
                "the question could not be handed to the device".into(),
            ));
        }
        Ok(answered.map(|answer| match answer {
            RemoteActionMessage::ConfirmationResponse { result, .. } => result,
            _ => Err(ActionHostError::Unavailable {
                reason: "the device did not answer in time".into(),
            }),
        }))
    }
}

/// Put `question` to the person at this device, one of their own, and
/// answer what they choose there: on the same "… wants to act for you" sheet
/// an agent's question is drawn on. `None` when this device is nobody's to
/// answer for — a question from a device of a person whose root this device
/// does not act for, about a room it does not hold, is not asked.
pub(crate) fn ask_here(
    net: &ShellNetwork,
    caller: &tilo_actions::remote::Caller,
    question: &ConfirmationQuestion,
) -> Option<Pending<Result<ConfirmationReply, ActionHostError>>> {
    let person = caller.user_public_key()?;
    // Only a device of this person's own asks them: the transport
    // authenticated it as one, and it acts for the root this device holds.
    if !net.holds_root(person) {
        return None;
    }
    let room = crate::shell::state::RoomId::parse(&question.room)?;
    net.workspace_of_room(room)?;
    let sheet = crate::network::agent_questions::Sheet::session();
    let ask = crate::network::agent_questions::request_for_foreign(question, &hex::encode(person));
    let handle = ask.handle.clone();
    let (completer, answer) = tilo_actions::remote::pending();
    let runtime = tokio::runtime::Handle::current();
    crate::shell::app_action_consent::notify_on(
        &sheet.queue,
        ask,
        Box::new(move |chosen| {
            runtime.spawn(async move {
                // The question shows no draft, so an answer changes nothing
                // in it.
                completer.complete(Ok(match chosen.await {
                    Ok(answer) if answer.allowed() => ConfirmationReply::AllowOnce,
                    Ok(_) | Err(_) => ConfirmationReply::Deny,
                }));
            });
            // The question is asked once, about one invocation: nothing of it
            // stays on the sheet after the person has answered.
            crate::shell::app_action_consent::withdraw_notice(&handle);
        }),
    );
    Some(answer)
}

impl ShellNetwork {
    /// Whether this device acts for `root`: the one that certified it, or
    /// one of its own workspaces'. A question about anybody else's room is
    /// not this device's person's to answer.
    pub(crate) fn holds_root(&self, root: &[u8; 32]) -> bool {
        self.person_roots().any(|own| own == *root)
    }
}
