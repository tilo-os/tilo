//! Answering what a person's agents on a host ask them.
//!
//! An agent a host runs for a person never confirms an action its grants
//! leave to them by its own word: the host asks the person's approving
//! devices about that one invocation ([`PendingConfirmation`]). A device or
//! page holding the person's root custody keeps an approver session with the
//! host ([`answer_questions`]): it puts every question the host lists on its
//! question sheet once, hands each answer back, and takes the sheet away
//! when the host stops listing the question (answered elsewhere, withdrawn
//! or expired).
//!
//! The session is the same everywhere; what reaches the host and what shows
//! the question differ. A device with its own certificate opens one approver
//! connection ([`Approver`]); a page holding the root signs each command
//! with it and asks over the session it already holds ([`MemberQuestions`]). A native session shows the question on its
//! sheet, a page on the same sheet over its desktop ([`QuestionSheet`]).

use std::collections::HashSet;
use std::future::Future;

use tilo_relay_protocol::certify::{
    ApproverReply, ApproverRequest, ConfirmationAnswer, PendingConfirmation,
};
use tokio::sync::mpsc;

use crate::certify::{Approver, MemberHost};
use crate::error::CertifyError;
use crate::signer::DeviceSigner;

/// One answer the person gave on the sheet: the question's id and the answer.
pub type Answered = ([u8; 32], ConfirmationAnswer);

/// Where a host's questions are read and answered.
pub trait QuestionHost {
    /// Every question the host waits on for this person.
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

    /// Take question `id` off the sheet: it was answered, or the host no
    /// longer asks it.
    fn withdraw(&mut self, id: [u8; 32]);
}

impl QuestionHost for Approver {
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

/// A host's questions to the member whose root a page holds: each command
/// signed by that root, asked where `host` reaches the host (the page's own
/// session).
pub struct MemberQuestions<'a, H> {
    pub host: &'a H,
    pub root: &'a dyn DeviceSigner,
}

impl<H: MemberHost> QuestionHost for MemberQuestions<'_, H> {
    async fn confirmations(&mut self) -> Result<Vec<PendingConfirmation>, CertifyError> {
        let approval =
            crate::pairing::member_approval(self.root, ApproverRequest::Confirmations).await?;
        match self.host.ask(&approval).await? {
            ApproverReply::Confirmations(asked) => Ok(asked),
            ApproverReply::Refused { reason } => Err(CertifyError::Denied(reason)),
            _ => Err(CertifyError::Denied(
                "the host answered something else".into(),
            )),
        }
    }

    async fn answer(
        &mut self,
        id: [u8; 32],
        answer: ConfirmationAnswer,
    ) -> Result<(), CertifyError> {
        let approval =
            crate::pairing::member_approval(self.root, ApproverRequest::Answer { id, answer })
                .await?;
        // A refusal says the question is gone: answered, withdrawn or expired.
        self.host.ask(&approval).await.map(drop)
    }
}

/// Show every question `host` lists on `sheet`, once, and hand each answer
/// arriving on `answers` back, until the host fails; `wait` is the pause
/// before the host is asked again. Questions the host stops listing leave
/// the sheet, and every question shown leaves it when the session ends.
pub async fn answer_questions<H, S, W>(
    host: &mut H,
    sheet: &mut S,
    answers: &mut mpsc::UnboundedReceiver<Answered>,
    mut wait: impl FnMut() -> W,
) -> CertifyError
where
    H: QuestionHost,
    S: QuestionSheet,
    W: Future<Output = ()>,
{
    let mut shown: HashSet<[u8; 32]> = HashSet::new();
    // The questions the person has answered, kept until the host stops
    // listing them: a list read while the answer is on its way does not put
    // the question back on their sheet.
    let mut done: HashSet<[u8; 32]> = HashSet::new();
    let failed = loop {
        let listed = match host.confirmations().await {
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
                if let Err(error) = host.answer(id, answer).await {
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
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
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

    /// A host listing, on each ask, the next of `lists`; failing once they run out.
    struct Scripted {
        lists: std::collections::VecDeque<Vec<PendingConfirmation>>,
        answered: Vec<Answered>,
    }

    impl QuestionHost for Scripted {
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
    /// when the host stops listing it (it expired, or another device
    /// answered), and every question still up leaves when the session ends.
    #[tokio::test]
    async fn a_question_shows_once_and_leaves_when_the_host_stops_asking() {
        let mut host = Scripted {
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
        let error = answer_questions(&mut host, &mut sheet, &mut answers, || {
            seen.push(());
            std::future::ready(())
        })
        .await;
        assert!(matches!(error, CertifyError::Denied(_)));
        assert_eq!(sheet.asked, 2, "each question is asked once");
        assert!(sheet.up.is_empty(), "nothing stays up: {:?}", sheet.up);
        assert_eq!(seen.len(), 4);
    }

    /// An answer goes back to the host, leaves the sheet, and a list read
    /// before the host took it does not put the question back up.
    #[tokio::test]
    async fn an_answer_goes_back_and_the_question_stays_answered() {
        let mut host = Scripted {
            lists: [vec![question(1)], vec![question(1)], vec![]].into(),
            answered: Vec::new(),
        };
        let mut sheet = Sheet::default();
        let (answered, mut answers) = mpsc::unbounded_channel();
        answered.send(([1; 32], ConfirmationAnswer::Deny)).unwrap();
        answer_questions(&mut host, &mut sheet, &mut answers, || {
            std::future::ready(())
        })
        .await;
        assert_eq!(host.answered, vec![([1; 32], ConfirmationAnswer::Deny)]);
        assert_eq!(sheet.asked, 1, "the answered question is not asked again");
        assert!(sheet.up.is_empty());
    }
}
