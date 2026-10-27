//! Admitting one invocation: the decision every door that runs an action
//! takes from the grants' answers, the same on every host.
//!
//! [`decide`](crate::decide) answers whether a party may take an action of
//! some kind at some node. Running one invocation weighs that answer with
//! who asks and how: someone acting for a person is asked before an action
//! its person attends, an ask goes to the person only where they can be
//! reached, and a record's own grants can ask or refuse as well.
//! [`admit`] is that weighing, so a broker admitting a caller's invocation,
//! a window checking its own and a page admitting its person's agree on
//! every answer.

use crate::Answer;

/// Who an invocation is admitted for, as admission weighs an ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asker {
    /// Whether the person can be asked to confirm before it runs: they act
    /// themselves, or the host reaches them on a device of theirs. Where
    /// nobody can be asked, an ask refuses.
    pub reachable: bool,
    /// Whether someone acts for the person (a delegated agent): they are
    /// asked before an action the person attends, whatever the grants
    /// allow.
    pub delegated: bool,
}

/// What the invocation is, as admission weighs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invoked {
    /// The grants' answer for the action at its node ([`crate::decide`]).
    pub answer: Answer,
    /// Whether the action needs its person attending.
    pub attended: bool,
    /// The answer for the record the invocation changes, where it is one.
    pub record: Option<Answer>,
}

/// How an invocation is admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// It runs.
    Runs,
    /// It runs once the person confirmed it.
    AsksFirst,
}

/// Why an invocation is refused: whose answer refused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The grants for the action itself, or an ask nobody can answer.
    Action,
    /// The grants for the record it changes.
    Record,
}

/// Admit `invoked` for `asker`.
///
/// # Errors
/// Whose answer refused it.
pub const fn admit(invoked: Invoked, asker: Asker) -> Result<Admission, Refused> {
    let mut asks = match invoked.answer {
        Answer::Allow => asker.delegated && invoked.attended,
        Answer::Ask if asker.reachable => true,
        Answer::Ask | Answer::Deny => return Err(Refused::Action),
    };
    match invoked.record {
        Some(Answer::Deny) => return Err(Refused::Record),
        Some(Answer::Ask) => asks = true,
        Some(Answer::Allow) | None => {}
    }
    Ok(if asks {
        Admission::AsksFirst
    } else {
        Admission::Runs
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWERS: [Answer; 3] = [Answer::Allow, Answer::Ask, Answer::Deny];

    fn invoked(answer: Answer) -> Invoked {
        Invoked {
            answer,
            attended: false,
            record: None,
        }
    }

    #[test]
    fn an_ask_goes_to_the_person_only_where_they_can_be_reached() {
        let person = Asker {
            reachable: true,
            delegated: false,
        };
        let unreachable = Asker {
            reachable: false,
            delegated: false,
        };
        assert_eq!(admit(invoked(Answer::Allow), person), Ok(Admission::Runs));
        assert_eq!(
            admit(invoked(Answer::Ask), person),
            Ok(Admission::AsksFirst)
        );
        assert_eq!(admit(invoked(Answer::Deny), person), Err(Refused::Action));
        assert_eq!(
            admit(invoked(Answer::Allow), unreachable),
            Ok(Admission::Runs)
        );
        assert_eq!(
            admit(invoked(Answer::Ask), unreachable),
            Err(Refused::Action)
        );
    }

    #[test]
    fn whoever_acts_for_the_person_asks_before_an_attended_action() {
        let delegate = Asker {
            reachable: true,
            delegated: true,
        };
        let attended = Invoked {
            attended: true,
            ..invoked(Answer::Allow)
        };
        assert_eq!(admit(attended, delegate), Ok(Admission::AsksFirst));
        assert_eq!(admit(invoked(Answer::Allow), delegate), Ok(Admission::Runs));
        let person = Asker {
            delegated: false,
            ..delegate
        };
        assert_eq!(admit(attended, person), Ok(Admission::Runs));
    }

    #[test]
    fn a_record_answer_asks_or_refuses_on_top() {
        let person = Asker {
            reachable: true,
            delegated: false,
        };
        for answer in ANSWERS {
            let record = Invoked {
                record: Some(answer),
                ..invoked(Answer::Allow)
            };
            let expected = match answer {
                Answer::Allow => Ok(Admission::Runs),
                Answer::Ask => Ok(Admission::AsksFirst),
                Answer::Deny => Err(Refused::Record),
            };
            assert_eq!(admit(record, person), expected);
        }
        let denied = Invoked {
            record: Some(Answer::Deny),
            ..invoked(Answer::Deny)
        };
        assert_eq!(
            admit(denied, person),
            Err(Refused::Action),
            "the action's own answer first"
        );
    }
}
