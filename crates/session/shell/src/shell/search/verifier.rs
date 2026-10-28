//! What a search row promises before a caller acts on it: the verifier the
//! system judges its own search surface with.
//!
//! A row is a promise — *this is the thing you named* — and the state behind
//! a promise is checked before the promise is kept. The verifier is the
//! placement of that check: rows arrive from many providers, an agent or a
//! script sees them without a screen, and both deserve the same answer. Its
//! verdict is read from what the system holds (the retained records, the
//! declared action parameters), never from what a provider or a run says
//! about itself.
//!
//! Two verdicts exist because there are two promises:
//!
//! - a row that says something exists — a record, a file — must name
//!   something that is still there ([`Verifier::verify_row`]);
//! - a row that can be invoked must carry what its action declared
//!   ([`Verifier::verify_arguments`]).
//!
//! The verifier is deliberately strict in one direction only: it refuses a
//! row whose subject the system can no longer observe, and it never widens
//! an action's declared parameters to let a caller through. Everything that
//! cannot be checked from here — a row whose subject lives behind another
//! machine, a provider with no verifiable state — comes back
//! [`Verdict::Unverifiable`] rather than a pass, because a check that cannot
//! observe its subject must not read as a success.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::params::{ActionArgs, ActionParam};
use ui::record::RecordId;

use crate::shell::room_records::{KnownRecord, RoomRecords};
use crate::shell::search::{ResourceKind, SearchResult};

/// Every way [`Verifier::verify_row`] can refuse a row: one per thing the
/// record corpus can say about the id a row names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowDefect {
    /// The id is not a record address at all, though the row claims to be
    /// one.
    NotARecordAddress,
    /// The address parses, but no room the system knows about holds or
    /// minted it: the row names something that was deleted, never existed,
    /// or belongs to a machine that has not answered yet.
    UnknownSubject,
    /// The row's provider does not claim a resource kind this verifier
    /// knows how to look up. A search row is only as good as the state
    /// behind it.
    UnsupportedKind,
}

impl RowDefect {
    /// The stable name of this defect, for receipts and logs.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NotARecordAddress => "not_a_record_address",
            Self::UnknownSubject => "unknown_subject",
            Self::UnsupportedKind => "unsupported_kind",
        }
    }
}

/// What the verifier says about one row, from the state the system holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The row names something the system can still observe.
    Passed,
    /// The row names nothing the system can observe.
    Failed(RowDefect),
    /// This verifier holds no state that could judge the row. Not a pass:
    /// no evidence is not evidence of agreement.
    Unverifiable,
}

impl Verdict {
    /// Whether this verdict is a pass.
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    /// The verdict as the row's caller reads it: `None` when it passed,
    /// the defect's stable name otherwise.
    pub const fn refusal(&self) -> Option<&'static str> {
        match self {
            Self::Passed | Self::Unverifiable => None,
            Self::Failed(defect) => Some(defect.reason()),
        }
    }
}

/// The verifier: the retained record corpus a row's subject is looked up in.
pub struct Verifier {
    records: Rc<RefCell<RoomRecords>>,
}

impl Verifier {
    /// A verifier over the corpus the shell already shares with its records
    /// search provider — the same state the rows came from, so a row and its
    /// verdict cannot disagree about which corpus is current.
    pub fn new(records: Rc<RefCell<RoomRecords>>) -> Self {
        Self { records }
    }

    /// Does `row` still name something the system holds?
    ///
    /// The check is one-directional and stateful on purpose: a record row
    /// whose id no room references any more is refused even though the
    /// provider that produced it still has it in a page, because what a
    /// caller acts on must be true now, not true when the page was built.
    pub fn verify_row(&self, row: &SearchResult) -> Verdict {
        if row.kind != ResourceKind::Record {
            return Verdict::Failed(RowDefect::UnsupportedKind);
        }
        self.verify_record(row.id.as_str())
    }

    /// [`Self::verify_row`] for a caller that holds only the address — the
    /// row's own Enter handler, bringing a record in — so a row and the
    /// action it runs judge the same subject the same way.
    pub fn verify_record(&self, id: &str) -> Verdict {
        let Some(wanted) = RecordId::parse(id) else {
            return Verdict::Failed(RowDefect::NotARecordAddress);
        };
        let held = self.records.borrow();
        if held.known().iter().any(|record| record.id == wanted) {
            Verdict::Passed
        } else {
            Verdict::Failed(RowDefect::UnknownSubject)
        }
    }

    /// Does an invocation of `action` carrying `arguments` say only what the
    /// action declared?
    ///
    /// This is [`ActionArgs::bind_json`] and nothing else: a missing
    /// required parameter, a wrong type, an unknown name and a value outside
    /// the declared choices are all refusals, so an invocation cannot be
    /// graded as done on arguments the action would never have accepted.
    pub fn verify_arguments(
        &self,
        params: &[ActionParam],
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionArgs, String> {
        ActionArgs::bind_json(params, arguments).map_err(|error| error.to_string())
    }

    /// The records the corpus holds, by id: what [`Self::verify_row`]
    /// answers from, for a caller that wants to say why a row was refused.
    pub fn known(&self) -> BTreeMap<RecordId, KnownRecord> {
        self.records
            .borrow()
            .known()
            .iter()
            .map(|record| (record.id.clone(), record.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::room_records::RoomRecords;
    use crate::shell::state::RoomId;
    use tilo_actions::params::ParamType;
    use ui::record::RecordKind;

    fn verifier() -> (Verifier, Rc<RefCell<RoomRecords>>, RoomId) {
        let room = RoomId::new();
        let records = Rc::new(RefCell::new(RoomRecords::in_memory()));
        records.borrow_mut().reference(
            room,
            &RecordId::for_file("/work/report.odt"),
            &RecordKind::new("document"),
        );
        (Verifier::new(Rc::clone(&records)), records, room)
    }

    fn record_row(id: &str) -> SearchResult {
        SearchResult::nullary(
            ResourceKind::Record,
            "records",
            id,
            "report.odt",
            None,
            0,
            |_, _, _| {},
        )
    }

    /// Positive: the row's subject is in the corpus, so the row is a promise
    /// the system can still keep.
    #[test]
    fn a_row_whose_record_a_room_holds_passes() {
        let (verifier, _records, _room) = verifier();
        let row = record_row(RecordId::for_file("/work/report.odt").as_str());
        assert_eq!(verifier.verify_row(&row), Verdict::Passed);
        assert!(verifier.verify_row(&row).passed());
        assert_eq!(verifier.verify_row(&row).refusal(), None);
    }

    /// Negative: a row that keeps naming a record after the last room
    /// dropped it is refused, though the page that produced it is unchanged.
    /// This is the "it is on the screen but not in the document" cheat.
    #[test]
    fn a_row_whose_record_no_room_holds_is_refused() {
        let (verifier, records, room) = verifier();
        let row = record_row(RecordId::for_file("/work/report.odt").as_str());
        assert!(
            records
                .borrow_mut()
                .unreference(room, &RecordId::parse(&row.id).unwrap())
        );
        assert_eq!(
            verifier.verify_row(&row),
            Verdict::Failed(RowDefect::UnknownSubject)
        );
        assert_eq!(
            verifier.verify_row(&row).refusal(),
            Some("unknown_subject"),
            "a caller has to learn why the row was refused"
        );
    }

    /// Negative: a row whose id is not an address at all is refused as such,
    /// rather than looked up in a corpus that could never contain it.
    #[test]
    fn a_row_that_names_no_record_address_is_refused() {
        let (verifier, _records, _room) = verifier();
        assert_eq!(
            verifier.verify_row(&record_row("not an address")),
            Verdict::Failed(RowDefect::NotARecordAddress)
        );
        assert_eq!(
            verifier.verify_record("not an address").refusal(),
            Some("not_a_record_address")
        );
    }

    /// Negative: a row of a kind the verifier cannot look up is refused
    /// rather than passed, because nothing observed it: no evidence is not
    /// evidence of agreement.
    #[test]
    fn a_row_of_an_unobserved_kind_is_refused_not_passed() {
        let (verifier, _records, _room) = verifier();
        let row = SearchResult::nullary(
            ResourceKind::Application,
            "applications",
            "so.tilo.notes",
            "Notes",
            None,
            0,
            |_, _, _| {},
        );
        assert_eq!(
            verifier.verify_row(&row),
            Verdict::Failed(RowDefect::UnsupportedKind)
        );
        assert!(!verifier.verify_row(&row).passed());
    }

    /// Positive: a call carrying exactly what the action declared binds.
    #[test]
    fn a_call_with_the_declared_arguments_verifies() {
        let (verifier, _records, _room) = verifier();
        let params = [ActionParam::required(
            "text",
            ParamType::Text,
            "What to type",
        )];
        let mut arguments = serde_json::Map::new();
        arguments.insert("text".into(), serde_json::json!("Quarterly Field Report"));
        let bound = verifier.verify_arguments(&params, &arguments).unwrap();
        assert_eq!(bound.text("text"), Some("Quarterly Field Report"));
    }

    /// Negative: padding, guessing and omitting are all refusals, so no
    /// invocation is graded as done on arguments the action never declared.
    #[test]
    fn a_call_outside_the_declared_parameters_is_refused() {
        let (verifier, _records, _room) = verifier();
        let params = [
            ActionParam::required("text", ParamType::Text, "What to type"),
            ActionParam::required("level", ParamType::Integer, "Heading level")
                .with_choices(["1", "2", "3"]),
        ];

        let mut padded = serde_json::Map::new();
        padded.insert("text".into(), serde_json::json!("Report"));
        padded.insert("level".into(), serde_json::json!(1));
        padded.insert("every_other_candidate".into(), serde_json::json!(true));
        assert!(
            verifier.verify_arguments(&params, &padded).is_err(),
            "an argument the action never declared must refuse the call: {:?}",
            verifier.verify_arguments(&params, &padded)
        );

        let mut out_of_choices = serde_json::Map::new();
        out_of_choices.insert("text".into(), serde_json::json!("Report"));
        out_of_choices.insert("level".into(), serde_json::json!(4));
        assert!(verifier.verify_arguments(&params, &out_of_choices).is_err());

        let mut missing = serde_json::Map::new();
        missing.insert("level".into(), serde_json::json!(1));
        assert!(verifier.verify_arguments(&params, &missing).is_err());

        let mut wrong_type = serde_json::Map::new();
        wrong_type.insert("text".into(), serde_json::json!(7));
        wrong_type.insert("level".into(), serde_json::json!(1));
        assert!(verifier.verify_arguments(&params, &wrong_type).is_err());
    }
}
