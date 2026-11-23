//! Answering what a person's agents on a server ask them.
//!
//! An agent a server runs for a person never confirms an action its grants
//! leave to them by its own word: the server asks the person's approving
//! devices about that one invocation ([`PendingConfirmation`]). A device or
//! page holding the person's root ownership keeps an approver session with the
//! server ([`answer_questions`]): it puts every question the server lists on its
//! question sheet once, hands each answer back, and takes the sheet away
//! when the server stops listing the question (answered elsewhere, withdrawn
//! or expired).
//!
//! The session is the same everywhere; what reaches the server and what shows
//! the question differ. A device with its own certificate opens one approver
//! connection ([`Approver`]); a page holding the root signs each command
//! with it and asks over the session it already holds ([`MemberQuestions`]). A native session shows the question on its
//! sheet, a page on the same sheet over its desktop ([`QuestionSheet`]).

use std::collections::HashSet;
use std::future::Future;

use tilo_server_protocol::certify::{
    ApproverReply, ApproverRequest, ConfirmationAnswer, PendingConfirmation,
};
use tokio::sync::mpsc;

#[cfg(not(target_arch = "wasm32"))]
use crate::certify::Approver;
use crate::certify::MemberServer;
use crate::error::CertifyError;
use crate::signer::DeviceSigner;

/// One answer the person gave on the sheet: the question's id and the answer.
pub type Answered = ([u8; 32], ConfirmationAnswer);

/// Where a server's questions are read and answered.
pub trait QuestionServer {
    /// Every question the server waits on for this person.
    fn confirmations(
        &mut self,
    ) -> impl Future<Output = Result<Vec<PendingConfirmation>, CertifyError>>;

    /// Answer question `id`. A question already gone is no failure.
    fn answer(
        &mut self,
        id: [u8; 32],
        answer: ConfirmationAnswer,
    ) -> impl Future<Output = Result<(), CertifyError>>;
}

/// What shows the person the questions.
pub trait QuestionSheet {
    /// Show `question`; the person's answer comes back on the session's
    /// answers ([`Answered`]).
    fn ask(&mut self, question: &PendingConfirmation);

    /// Take question `id` off the sheet: it was answered, or the server no
    /// longer asks it.
    fn withdraw(&mut self, id: [u8; 32]);
}

#[cfg(not(target_arch = "wasm32"))]
impl QuestionServer for Approver {
    async fn confirmations(&mut self) -> Result<Vec<PendingConfirmation>, CertifyError> {
        self.list_confirmations().await
    }

    async fn answer(
        &mut self,
        id: [u8; 32],
        answer: ConfirmationAnswer,
    ) -> Result<(), CertifyError> {
        Self::answer(self, id, answer).await.map(drop)
    }
}

/// A server's questions to a member, from a page of theirs: each command
/// signed by their root where the page holds it, else by the page's own
/// device (`device`, its certificate), asked where `server` reaches the server
/// (the page's own session).
pub struct MemberQuestions<'a, H> {
    pub server: &'a H,
    /// What signs: the person's root, or the device `device` certifies.
    pub root: &'a dyn DeviceSigner,
    /// The page's device certificate when `root` is that device's key.
    pub device: Option<&'a tilo_user_identity::DeviceCert>,
}

impl<'a, H> MemberQuestions<'a, H> {
    /// The questions for the person whose root is `root`, each command signed by
    /// it, asked where `server` reaches: what a computer holding the root
    /// answers its own agent's questions with.
    pub fn of_root(server: &'a H, root: &'a dyn DeviceSigner) -> Self {
        Self {
            server,
            root,
            device: None,
        }
    }
}

impl<H: MemberServer> QuestionServer for MemberQuestions<'_, H> {
    async fn confirmations(&mut self) -> Result<Vec<PendingConfirmation>, CertifyError> {
        let approval = crate::pairing::member_approval_as(
            self.root,
            self.device,
            ApproverRequest::Confirmations,
        )
        .await?;
        match self.server.ask(&approval).await? {
            ApproverReply::Confirmations(asked) => Ok(asked),
            ApproverReply::Refused { reason } => Err(CertifyError::Denied(reason)),
            _ => Err(CertifyError::Denied(
                "the server answered something else".into(),
            )),
        }
    }

    async fn answer(
        &mut self,
        id: [u8; 32],
        answer: ConfirmationAnswer,
    ) -> Result<(), CertifyError> {
        let approval = crate::pairing::member_approval_as(
            self.root,
            self.device,
            ApproverRequest::Answer { id, answer },
        )
        .await?;
        // A refusal says the question is gone: answered, withdrawn or expired.
        self.server.ask(&approval).await.map(drop)
    }
}

impl<H: MemberServer> MemberQuestions<'_, H> {
    /// The key the server signs its pushes with, which a browser subscribes
    /// with; empty where the server pushes nothing.
    ///
    /// # Errors
    /// The server was not reached, or refused.
    pub async fn push_key(&self) -> Result<Vec<u8>, CertifyError> {
        let approval =
            crate::pairing::member_approval_as(self.root, self.device, ApproverRequest::PushKey)
                .await?;
        match self.server.ask(&approval).await? {
            ApproverReply::PushKey(key) => Ok(key),
            ApproverReply::Refused { reason } => Err(CertifyError::Denied(reason)),
            _ => Err(CertifyError::Denied(
                "the server answered something else".into(),
            )),
        }
    }

    /// Have the server push this browser (`subscription`) a word when an
    /// agent waits for the person and no Tilo of theirs is open.
    ///
    /// # Errors
    /// The server was not reached, or did not keep it.
    pub async fn notify_at(
        &self,
        subscription: tilo_server_protocol::certify::PushSubscription,
    ) -> Result<(), CertifyError> {
        let approval = crate::pairing::member_approval_as(
            self.root,
            self.device,
            ApproverRequest::Notify(subscription),
        )
        .await?;
        match self.server.ask(&approval).await? {
            ApproverReply::Decided => Ok(()),
            ApproverReply::Refused { reason } => Err(CertifyError::Denied(reason)),
            _ => Err(CertifyError::Denied(
                "the server answered something else".into(),
            )),
        }
    }
}

/// Show every question `server` lists on `sheet`, once, and hand each answer
/// arriving on `answers` back, until the server fails; `wait` is the pause
/// before the server is asked again. Questions the server stops listing leave
/// the sheet, and every question shown leaves it when the session ends.
pub async fn answer_questions<H, S, W>(
    server: &mut H,
    sheet: &mut S,
    answers: &mut mpsc::UnboundedReceiver<Answered>,
    mut wait: impl FnMut() -> W,
) -> CertifyError
where
    H: QuestionServer,
    S: QuestionSheet,
    W: Future<Output = ()>,
{
    let mut shown: HashSet<[u8; 32]> = HashSet::new();
    // The questions the person has answered, kept until the server stops
    // listing them: a list read while the answer is on its way does not put
    // the question back on their sheet.
    let mut done: HashSet<[u8; 32]> = HashSet::new();
    let failed = loop {
        let listed = match server.confirmations().await {
            Ok(listed) => listed,
            Err(error) => break error,
        };
        let live: HashSet<[u8; 32]> = listed.iter().map(|question| question.id).collect();
        done.retain(|id| live.contains(id));
        let gone: Vec<[u8; 32]> = shown.difference(&live).copied().collect();
        for id in gone {
            shown.remove(&id);
            sheet.withdraw(id);
        }
        for question in &listed {
            if !done.contains(&question.id) && shown.insert(question.id) {
                sheet.ask(question);
            }
        }
        tokio::select! {
            biased;
            Some((id, answer)) = answers.recv() => {
                shown.remove(&id);
                sheet.withdraw(id);
                done.insert(id);
                if let Err(error) = server.answer(id, answer).await {
                    break error;
                }
            }
            () = wait() => {}
        }
    };
    for id in shown {
        sheet.withdraw(id);
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: u8) -> PendingConfirmation {
        PendingConfirmation {
            id: [id; 32],
            agent_id: "visa-agent".into(),
            agent_label: "Visa backend #1".into(),
            action_label: "submit the application".into(),
            resource_label: "application.pdf".into(),
            risk: "sensitive".into(),
            workspace: "Applicant: Jane Doe".into(),
            expires_at_ns: 0,
            ..PendingConfirmation::default()
        }
    }

    /// A server listing, on each ask, the next of `lists`; failing once they run out.
    struct Scripted {
        lists: std::collections::VecDeque<Vec<PendingConfirmation>>,
        answered: Vec<Answered>,
    }

    impl QuestionServer for Scripted {
        async fn confirmations(&mut self) -> Result<Vec<PendingConfirmation>, CertifyError> {
            self.lists
                .pop_front()
                .ok_or_else(|| CertifyError::Denied("gone".into()))
        }

        async fn answer(
            &mut self,
            id: [u8; 32],
            answer: ConfirmationAnswer,
        ) -> Result<(), CertifyError> {
            self.answered.push((id, answer));
            Ok(())
        }
    }

    #[derive(Default)]
    struct Sheet {
        up: Vec<[u8; 32]>,
        asked: usize,
    }

    impl QuestionSheet for Sheet {
        fn ask(&mut self, question: &PendingConfirmation) {
            self.asked = self.asked.saturating_add(1);
            self.up.push(question.id);
        }

        fn withdraw(&mut self, id: [u8; 32]) {
            self.up.retain(|up| *up != id);
        }
    }

    /// A question shows once however often it is listed, leaves the sheet
    /// when the server stops listing it (it expired, or another device
    /// answered), and every question still up leaves when the session ends.
    #[tokio::test]
    async fn a_question_shows_once_and_leaves_when_the_host_stops_asking() {
        let mut server = Scripted {
            lists: [
                vec![question(1)],
                vec![question(1), question(2)],
                vec![question(2)],
                vec![question(2)],
            ]
            .into(),
            answered: Vec::new(),
        };
        let mut sheet = Sheet::default();
        let (_answered, mut answers) = mpsc::unbounded_channel();
        let mut seen = Vec::new();
        let error = answer_questions(&mut server, &mut sheet, &mut answers, || {
            seen.push(());
            std::future::ready(())
        })
        .await;
        assert!(matches!(error, CertifyError::Denied(_)));
        assert_eq!(sheet.asked, 2, "each question is asked once");
        assert!(sheet.up.is_empty(), "nothing stays up: {:?}", sheet.up);
        assert_eq!(seen.len(), 4);
    }

    /// An answer goes back to the server, leaves the sheet, and a list read
    /// before the server took it does not put the question back up.
    #[tokio::test]
    async fn an_answer_goes_back_and_the_question_stays_answered() {
        let mut server = Scripted {
            lists: [vec![question(1)], vec![question(1)], vec![]].into(),
            answered: Vec::new(),
        };
        let mut sheet = Sheet::default();
        let (answered, mut answers) = mpsc::unbounded_channel();
        answered.send(([1; 32], ConfirmationAnswer::Deny)).unwrap();
        answer_questions(&mut server, &mut sheet, &mut answers, || {
            std::future::ready(())
        })
        .await;
        assert_eq!(server.answered, vec![([1; 32], ConfirmationAnswer::Deny)]);
        assert_eq!(sheet.asked, 1, "the answered question is not asked again");
        assert!(sheet.up.is_empty());
    }
}
