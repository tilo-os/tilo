//! When a generic field edit asks for confirmation.
//!
//! Every door decides it the same way, from the edit alone: no app code is
//! needed to read a field edit. Removing an item of a collection asks, as a
//! delete verb does, unless the record publishes that collection's removals
//! as quiet. Any edit of a field the record publishes as guarded asks: one
//! whose change can weaken the person's security or expose them. A field
//! that weakens only one way ([`Weaker`]) asks only for a change that way.
//! Otherwise setting, splicing, adding, upserting an item by its key, moving
//! and an outbox's `settle` and `forget` ask nothing.

/// The tag a published field edit carries.
pub const FIELD_EDIT_TAG: &str = "confirmation:field-edit";

/// The tag prefix naming one collection, by its dotted path from the edit's
/// root (`*` for an item), whose removals ask nothing.
pub const QUIET_REMOVAL_TAG: &str = "confirmation:quiet-removal:";

/// The tag prefix naming one field, by its dotted path from the edit's root
/// (`*` for an item), any edit at or below which asks.
pub const GUARDED_FIELD_TAG: &str = "confirmation:guarded-field:";

/// The tag prefix of a field that weakens security only one way
/// ([`weakening_tag`]): an edit making it weaker asks, any other does not.
pub const WEAKENING_FIELD_TAG: &str = "confirmation:weakening-field:";

/// Which way a field's value weakens the person's security.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Weaker<'a> {
    /// A higher number is weaker (a lock's delay); `weakest`, when set, is
    /// weaker than any (the delay's `0`, never).
    Higher { weakest: Option<i64> },
    /// A lower number is weaker; `weakest`, when set, is weaker than any.
    Lower { weakest: Option<i64> },
    /// The values, as JSON text, from the strongest to the weakest (`true`
    /// then `false` for a switch that protects when on); any other value is
    /// weaker than all of them.
    Order(&'a [&'a str]),
}

impl Weaker<'_> {
    /// How weak `value` (JSON text) is: higher is weaker, `None` when it is
    /// not a value this ranks.
    fn rank(&self, value: &str) -> Option<f64> {
        let number = |weakest: Option<i64>, sign: f64| {
            let number: f64 = value.parse().ok()?;
            #[allow(clippy::cast_precision_loss, reason = "a sentinel such as 0 or -1")]
            let weakest = weakest.map(|weakest| weakest as f64);
            Some(if weakest == Some(number) {
                f64::INFINITY
            } else {
                number * sign
            })
        };
        match *self {
            Self::Higher { weakest } => number(weakest, 1.0),
            Self::Lower { weakest } => number(weakest, -1.0),
            Self::Order(order) => {
                let at = order.iter().position(|known| *known == value);
                #[allow(clippy::cast_precision_loss, reason = "a handful of values")]
                Some(at.map_or(f64::INFINITY, |at| at as f64))
            }
        }
    }

    /// Whether `next` is weaker than `now`, both JSON text. Not knowing the
    /// value now, or either value not being one this ranks, counts as
    /// weaker.
    #[must_use]
    pub fn weakens(&self, now: Option<&str>, next: &str) -> bool {
        match (now.and_then(|now| self.rank(now)), self.rank(next)) {
            (Some(now), Some(next)) => next > now,
            _ => true,
        }
    }
}

/// The [`WEAKENING_FIELD_TAG`] publishing that the field at `path` (dotted,
/// as [`dotted`] spells it) weakens `weaker`'s way and holds `now` (JSON
/// text), when known.
#[must_use]
pub fn weakening_tag(path: &str, now: Option<&str>, weaker: Weaker<'_>) -> String {
    let mut tag = format!("{WEAKENING_FIELD_TAG}{path}\t{}\t", now.unwrap_or_default());
    let bound = |tag: &mut String, name: &str, weakest: Option<i64>| {
        tag.push_str(name);
        if let Some(weakest) = weakest {
            tag.push('\t');
            tag.push_str(&weakest.to_string());
        }
    };
    match weaker {
        Weaker::Higher { weakest } => bound(&mut tag, "higher", weakest),
        Weaker::Lower { weakest } => bound(&mut tag, "lower", weakest),
        Weaker::Order(order) => {
            tag.push_str("order");
            for value in order {
                tag.push('\t');
                tag.push_str(value);
            }
        }
    }
    tag
}

/// A field that weakens one way, read back from its [`weakening_tag`]
/// (without the prefix). JSON text holds no raw tab, so tabs part it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeakeningField<'t> {
    /// Its dotted path.
    pub path: &'t str,
    /// Its value now, as JSON text, when known.
    pub now: Option<&'t str>,
    rule: &'t str,
    values: Vec<&'t str>,
}

impl<'t> WeakeningField<'t> {
    /// The field `text` (a tag after [`WEAKENING_FIELD_TAG`]) publishes.
    #[must_use]
    pub fn parse(text: &'t str) -> Option<Self> {
        let mut parts = text.split('\t');
        let path = parts.next()?;
        let now = parts.next()?;
        let rule = parts.next()?;
        Some(Self {
            path,
            now: (!now.is_empty()).then_some(now),
            rule,
            values: parts.collect(),
        })
    }

    /// Which way it weakens; `None` for a rule this version does not know.
    #[must_use]
    pub fn weaker(&self) -> Option<Weaker<'_>> {
        let weakest = || match self.values.as_slice() {
            [] => Some(None),
            [weakest] => weakest.parse().ok().map(Some),
            _ => None,
        };
        match self.rule {
            "higher" => weakest().map(|weakest| Weaker::Higher { weakest }),
            "lower" => weakest().map(|weakest| Weaker::Lower { weakest }),
            "order" => Some(Weaker::Order(&self.values)),
            _ => None,
        }
    }
}

/// Whether the field edit `op` at `path`, setting `value` (JSON text) when
/// it is a set, can weaken one of `fields`: a set of the field itself to a
/// weaker value, and any other edit at or below it, which is not read.
#[must_use]
pub fn weakening_edit(
    op: &str,
    path: &[EditStep<'_>],
    value: Option<&str>,
    fields: &[WeakeningField<'_>],
) -> bool {
    let at = dotted(path);
    fields.iter().any(|field| {
        if !guarded_at(path, &[field.path]) {
            return false;
        }
        match (op, value, field.weaker()) {
            ("set", Some(value), Some(weaker)) if at == field.path => {
                weaker.weakens(field.now, value)
            }
            _ => true,
        }
    })
}

/// One step of a field edit's path, as the decision reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditStep<'a> {
    /// A field of a record, by its key.
    Field(&'a str),
    /// An item, an entry or a cell, whichever it is.
    Item,
}

/// `path` as the dotted pattern [`QUIET_REMOVAL_TAG`] names collections by.
#[must_use]
pub fn dotted(path: &[EditStep<'_>]) -> String {
    path.iter()
        .map(|step| match step {
            EditStep::Field(key) => *key,
            EditStep::Item => "*",
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Whether the field edit `op` (its JSON name) at `path` asks for
/// confirmation before it runs, where `quiet` names the collections whose
/// removals ask nothing and `guarded` the fields whose every edit asks.
#[must_use]
pub fn field_edit_asks(op: &str, path: &[EditStep<'_>], quiet: &[&str], guarded: &[&str]) -> bool {
    guarded_at(path, guarded) || (op == "remove-item" && !quiet.contains(&dotted(path).as_str()))
}

/// Whether `path` is one of the `guarded` fields or lies below one.
#[must_use]
pub fn guarded_at(path: &[EditStep<'_>], guarded: &[&str]) -> bool {
    (1..=path.len())
        .filter_map(|len| path.get(..len))
        .any(|prefix| guarded.contains(&dotted(prefix).as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loud_collections_removal_asks() {
        let notes = [EditStep::Field("notes")];
        let tags = [
            EditStep::Field("notes"),
            EditStep::Item,
            EditStep::Field("tags"),
        ];
        let quiet = ["notes.*.tags"];
        assert!(field_edit_asks("remove-item", &notes, &quiet, &[]));
        assert!(!field_edit_asks("remove-item", &tags, &quiet, &[]));
        for op in [
            "set",
            "splice",
            "insert-item",
            "upsert-item",
            "move-to",
            "settle",
            "forget",
            "remove-key",
        ] {
            assert!(!field_edit_asks(op, &notes, &quiet, &[]), "{op}");
        }
        assert_eq!(dotted(&tags), "notes.*.tags");
    }

    #[test]
    fn every_edit_at_or_below_a_guarded_field_asks() {
        let lock = [EditStep::Field("lock")];
        let delay = [EditStep::Field("lock"), EditStep::Field("delay")];
        let other = [EditStep::Field("locked")];
        let guarded = ["lock"];
        for op in ["set", "splice", "insert-item", "remove-item"] {
            assert!(field_edit_asks(op, &lock, &[], &guarded), "{op}");
            assert!(field_edit_asks(op, &delay, &[], &guarded), "{op}");
        }
        assert!(!field_edit_asks("set", &other, &[], &guarded));
        assert!(
            field_edit_asks("remove-item", &lock, &["lock"], &guarded),
            "a quiet removal of a guarded field still asks"
        );
    }

    #[test]
    fn a_number_weakens_its_way_and_its_weakest_value_beats_all() {
        let delay = Weaker::Higher { weakest: Some(0) };
        assert!(!delay.weakens(Some("5"), "1"), "shorter is stricter");
        assert!(!delay.weakens(Some("5"), "5"));
        assert!(delay.weakens(Some("5"), "10"));
        assert!(delay.weakens(Some("60"), "0"), "never is the weakest");
        assert!(!delay.weakens(Some("0"), "60"), "any delay beats never");
        assert!(delay.weakens(None, "1"), "not knowing the value now asks");
        assert!(
            delay.weakens(Some("5"), "\"soon\""),
            "an unranked value asks"
        );
        let floor = Weaker::Lower { weakest: None };
        assert!(floor.weakens(Some("8"), "4"));
        assert!(!floor.weakens(Some("8"), "12"));
    }

    #[test]
    fn a_switch_and_a_choice_weaken_down_their_order() {
        let shown = Weaker::Order(&["false", "true"]);
        assert!(shown.weakens(Some("false"), "true"));
        assert!(!shown.weakens(Some("true"), "false"));
        let zone = Weaker::Order(&["\"drop\"", "\"public\"", "\"trusted\""]);
        assert!(zone.weakens(Some("\"public\""), "\"trusted\""));
        assert!(!zone.weakens(Some("\"public\""), "\"drop\""));
        assert!(zone.weakens(Some("\"drop\""), "\"other\""));
    }

    #[test]
    fn a_published_weakening_field_reads_back_and_decides_its_edits() {
        let order = ["false", "true"];
        let tags = [
            weakening_tag("lock", Some("5"), Weaker::Higher { weakest: Some(0) }),
            weakening_tag("shown", Some("false"), Weaker::Order(&order)),
            weakening_tag("rules.*.on", None, Weaker::Order(&order)),
        ];
        let fields: Vec<WeakeningField<'_>> = tags
            .iter()
            .map(|tag| WeakeningField::parse(tag.strip_prefix(WEAKENING_FIELD_TAG).unwrap()))
            .collect::<Option<_>>()
            .unwrap();
        assert_eq!(
            fields[0].weaker(),
            Some(Weaker::Higher { weakest: Some(0) })
        );
        assert_eq!(fields[1].weaker(), Some(Weaker::Order(&order)));
        let lock = [EditStep::Field("lock")];
        let shown = [EditStep::Field("shown")];
        let rule = [
            EditStep::Field("rules"),
            EditStep::Item,
            EditStep::Field("on"),
        ];
        assert!(!weakening_edit("set", &lock, Some("1"), &fields));
        assert!(weakening_edit("set", &lock, Some("0"), &fields));
        assert!(weakening_edit("splice", &lock, None, &fields));
        assert!(weakening_edit("set", &shown, Some("true"), &fields));
        assert!(!weakening_edit("set", &shown, Some("false"), &fields));
        assert!(
            weakening_edit("set", &rule, Some("false"), &fields),
            "an item's value is not published, so any edit asks"
        );
        assert!(!weakening_edit(
            "set",
            &[EditStep::Field("other")],
            Some("1"),
            &fields
        ));
    }
}
