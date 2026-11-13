//! Predicates over the fields of a resource, which a store answers itself from its index's filter
//! columns where it keeps one (search.md, "Who indexes").
//!
//! `items.filter(x -> not x.done)` and `items.count(x -> x.pinned)` over a collection field, a
//! type's named filters (`filter "Open" = not done`) and the query a `query(T, ...)` asks compile to
//! a [`Pred`] as well as to their loop. In a view the host hands the predicate to the store, which
//! lists the matching items without the loop running; elsewhere the loop runs.
//!
//! The compiler writes field operands as field ids ([`Pred<String>`]); the host resolves them to
//! what the store reads.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pred<F> {
    Const(bool),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    Cmp(CmpOp, Operand<F>, Operand<F>),
    Has(F, Lit),
    /// The field (an identifier, or a set of them) holds one that matches the pattern, `*` standing
    /// for any text (see [`crate::identifier::matches`]).
    Like(F, String),
    /// The field holds text a person searched for by a word of it: a case-insensitive match
    /// anywhere inside it. Free text as search reads it, where [`Like`](Self::Like) is a pattern
    /// written with `*`.
    Text(F, String),
    Present(F),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// One side of a comparison: a field of the item, a literal, or how many a field holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operand<F> {
    Field(F),
    Lit(Lit),
    Len(F),
    /// The `n`th value the code that asked holds (`c -> c.column == col`): bound when the predicate
    /// is asked ([`Pred::bind`]), and a store answers it as the literal.
    Arg(u32),
    /// The time the predicate is asked at, `offset` seconds on: the day (`today + 1d`) when `day`,
    /// else the instant (`now - 7d`). Read when the predicate is asked ([`Pred::at`]), so a named
    /// filter on it ("Today": `due <= today`) is answered from the field's column then.
    Clock {
        day: bool,
        offset: i64,
    },
    /// The person the predicate is asked for (`workspace.me` in a named filter). Read when the
    /// predicate is asked ([`Pred::viewer`]), so a filter naming them ("Assigned to me") reads
    /// differently for each of the people a workspace is shared with.
    Viewer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockReads {
    /// The person's day (`today`, `today + 7d`): read at their day, it reads the same until their
    /// next midnight.
    pub day: bool,
    /// The instant (`now - 7d`): it reads differently as the clock runs.
    pub instant: bool,
}

/// A value a predicate compares.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lit {
    Bool(bool),
    Int(i64),
    Text(String),
    Float(Float),
    Date(String),
    Time(String),
    Variant(String),
    Ref(String),
    /// An identifier as it is held, `scheme:value`.
    Ident(String),
    /// No value: an option that is none.
    Absent,
    Set(Vec<Lit>),
    Principal(String),
}

/// A Float a predicate compares: equal, ordered and hashed by its value, `-0.0` as `0.0`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Float(pub f64);

impl PartialEq for Float {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Float {}

impl PartialOrd for Float {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Float {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.0 + 0.0).total_cmp(&(other.0 + 0.0))
    }
}

impl std::hash::Hash for Float {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.0 + 0.0).to_bits().hash(state);
    }
}

impl Lit {
    /// The value a register holds as JSON (an option, an enum, a reference or a scalar), as a
    /// predicate compares it; `None` for one it does not.
    #[must_use]
    pub fn of_json(json: &serde_json::Value) -> Option<Self> {
        use serde_json::Value as J;
        Some(match json {
            J::Null => Self::Absent,
            J::Bool(b) => Self::Bool(*b),
            J::Number(n) => match n.as_i64() {
                Some(i) => Self::Int(i),
                None => Self::Float(Float(n.as_f64()?)),
            },
            J::String(s) => Self::Text(s.clone()),
            J::Object(m) => {
                if let Some(inner) = m.get("some") {
                    return Self::of_json(inner);
                }
                if m.contains_key("none") {
                    return Some(Self::Absent);
                }
                if let Some(J::String(v)) = m.get("variant") {
                    return Some(Self::Variant(v.clone()));
                }
                if let Some(J::String(r)) = m.get("ref") {
                    return Some(Self::Ref(r.clone()));
                }
                if let Some(J::String(p)) = m.get("principal") {
                    return Some(Self::Principal(p.clone()));
                }
                return None;
            }
            J::Array(_) => return None,
        })
    }

    /// The text a text-like value is held as: a text, a date, a time or an identifier.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(t) | Self::Date(t) | Self::Time(t) | Self::Ident(t) => Some(t),
            _ => None,
        }
    }

    #[must_use]
    pub fn member_text(&self) -> Option<String> {
        match self {
            Self::Int(i) => Some(i.to_string()),
            Self::Float(f) => Some(f.0.to_string()),
            Self::Bool(b) => Some(b.to_string()),
            Self::Variant(v) | Self::Ref(v) | Self::Principal(v) => Some(v.clone()),
            other => other.as_text().map(str::to_owned),
        }
    }

    /// How `self` and `other` compare under `op`: `None` when values of their kinds do not compare
    /// that way. An absent value is equal only to another and orders with nothing.
    #[must_use]
    pub fn compare(&self, op: CmpOp, other: &Self) -> Option<bool> {
        use std::cmp::Ordering;
        let ordered = !matches!(op, CmpOp::Eq | CmpOp::Ne);
        let ord: Ordering = match (self, other) {
            (Self::Absent, Self::Absent) => Ordering::Equal,
            (Self::Absent, _) | (_, Self::Absent) => {
                return Some(matches!(op, CmpOp::Ne));
            }
            (Self::Bool(x), Self::Bool(y)) if !ordered => x.cmp(y),
            (Self::Int(x), Self::Int(y)) => x.cmp(y),
            (Self::Float(x), Self::Float(y)) => x.cmp(y),
            (Self::Int(x), Self::Float(y)) => Float(*x as f64).cmp(y),
            (Self::Float(x), Self::Int(y)) => x.cmp(&Float(*y as f64)),
            (Self::Variant(x), Self::Variant(y))
            | (Self::Ref(x), Self::Ref(y))
            | (Self::Principal(x), Self::Principal(y))
                if !ordered =>
            {
                x.cmp(y)
            }
            (Self::Ident(x), y) | (y, Self::Ident(x)) if !ordered => {
                let y = y.as_text()?;
                x.to_lowercase().cmp(&y.to_lowercase())
            }
            (x, y) => {
                let (a, b) = (x.as_text()?, y.as_text()?);
                // Dates and times order chronologically as ISO text; a time against a day compares by
                // the day.
                let dated = matches!(x, Self::Date(_) | Self::Time(_))
                    || matches!(y, Self::Date(_) | Self::Time(_));
                if ordered && !dated {
                    return None;
                }
                if matches!(x, Self::Date(_)) != matches!(y, Self::Date(_)) && dated {
                    let day = |t: &str| t.get(..10).unwrap_or(t).to_owned();
                    day(a).cmp(&day(b))
                } else {
                    a.cmp(b)
                }
            }
        };
        Some(match op {
            CmpOp::Eq => ord.is_eq(),
            CmpOp::Ne => ord.is_ne(),
            CmpOp::Lt => ord.is_lt(),
            CmpOp::Le => ord.is_le(),
            CmpOp::Gt => ord.is_gt(),
            CmpOp::Ge => ord.is_ge(),
        })
    }
}

impl<F> Pred<F> {
    /// The same predicate with each field `f` resolved to `map(f)`; `None` when one does not
    /// resolve.
    pub fn resolve<G>(&self, map: &mut impl FnMut(&F) -> Option<G>) -> Option<Pred<G>> {
        Some(match self {
            Self::Const(b) => Pred::Const(*b),
            Self::Not(p) => Pred::Not(Box::new(p.resolve(map)?)),
            Self::And(ps) => Pred::And(ps.iter().map(|p| p.resolve(map)).collect::<Option<_>>()?),
            Self::Or(ps) => Pred::Or(ps.iter().map(|p| p.resolve(map)).collect::<Option<_>>()?),
            Self::Cmp(op, a, b) => Pred::Cmp(*op, a.resolve(map)?, b.resolve(map)?),
            Self::Has(f, v) => Pred::Has(map(f)?, v.clone()),
            Self::Like(f, p) => Pred::Like(map(f)?, p.clone()),
            Self::Text(f, p) => Pred::Text(map(f)?, p.clone()),
            Self::Present(f) => Pred::Present(map(f)?),
        })
    }

    /// The same predicate with each value the asking code holds ([`Operand::Arg`]) as `args` holds
    /// at its place.
    #[must_use]
    pub fn bind(&self, args: &[Lit]) -> Option<Self>
    where
        F: Clone,
    {
        Some(match self {
            Self::Not(p) => Self::Not(Box::new(p.bind(args)?)),
            Self::And(ps) => Self::And(ps.iter().map(|p| p.bind(args)).collect::<Option<_>>()?),
            Self::Or(ps) => Self::Or(ps.iter().map(|p| p.bind(args)).collect::<Option<_>>()?),
            Self::Cmp(op, a, b) => Self::Cmp(*op, a.bind(args)?, b.bind(args)?),
            other => other.clone(),
        })
    }

    /// The same predicate with each [`Operand::Clock`] read at `now`, seconds since 1970 (UTC), by
    /// someone whose clock is `zone` seconds ahead of UTC.
    #[must_use]
    pub fn at(&self, now: i64, zone: i64) -> Self
    where
        F: Clone,
    {
        match self {
            Self::Not(p) => Self::Not(Box::new(p.at(now, zone))),
            Self::And(ps) => Self::And(ps.iter().map(|p| p.at(now, zone)).collect()),
            Self::Or(ps) => Self::Or(ps.iter().map(|p| p.at(now, zone)).collect()),
            Self::Cmp(op, a, b) => Self::Cmp(*op, a.at(now, zone), b.at(now, zone)),
            other => other.clone(),
        }
    }

    /// The fields it compares with the clock ([`Operand::Clock`]), each with whether it reads the
    /// day or the instant, and by how much it offsets the clock.
    #[must_use]
    pub fn clock_fields(&self) -> Vec<(&F, bool, i64)> {
        let mut out = Vec::new();
        self.each_clock_field(&mut |field, day, offset| out.push((field, day, offset)));
        out
    }

    fn each_clock_field<'a>(&'a self, f: &mut impl FnMut(&'a F, bool, i64)) {
        match self {
            Self::Not(p) => p.each_clock_field(f),
            Self::And(ps) | Self::Or(ps) => ps.iter().for_each(|p| p.each_clock_field(f)),
            Self::Cmp(_, a, b) => {
                // Either side may be the field: a filter may ask `f <= now` or `now <= f`.
                let (field, clock) = match (a, b) {
                    (Operand::Field(field), Operand::Clock { day, offset })
                    | (Operand::Clock { day, offset }, Operand::Field(field)) => {
                        (field, (*day, *offset))
                    }
                    _ => return,
                };
                f(field, clock.0, clock.1);
            }
            _ => {}
        }
    }

    #[must_use]
    pub fn reads_clock(&self) -> bool {
        let reads = self.clock_reads();
        reads.day || reads.instant
    }

    /// The same predicate with each [`Operand::Viewer`] as the person `who` names: whose list it is.
    #[must_use]
    pub fn viewer(&self, who: &str) -> Self
    where
        F: Clone,
    {
        match self {
            Self::Not(p) => Self::Not(Box::new(p.viewer(who))),
            Self::And(ps) => Self::And(ps.iter().map(|p| p.viewer(who)).collect()),
            Self::Or(ps) => Self::Or(ps.iter().map(|p| p.viewer(who)).collect()),
            Self::Cmp(op, a, b) => Self::Cmp(*op, a.viewer(who), b.viewer(who)),
            other => other.clone(),
        }
    }

    #[must_use]
    pub fn reads_viewer(&self) -> bool {
        let mut reads = false;
        self.each_viewer(&mut || reads = true);
        reads
    }

    fn each_viewer(&self, f: &mut impl FnMut()) {
        match self {
            Self::Not(p) => p.each_viewer(f),
            Self::And(ps) | Self::Or(ps) => ps.iter().for_each(|p| p.each_viewer(f)),
            Self::Cmp(_, a, b) => {
                for side in [a, b] {
                    if matches!(side, Operand::Viewer) {
                        f();
                    }
                }
            }
            _ => {}
        }
    }

    /// What of the time it is asked at it reads: what [`Self::at`] makes of it changes only when
    /// that does.
    #[must_use]
    pub fn clock_reads(&self) -> ClockReads {
        let mut reads = ClockReads::default();
        self.each_clock(&mut |day| {
            if day {
                reads.day = true;
            } else {
                reads.instant = true;
            }
        });
        reads
    }

    fn each_clock(&self, f: &mut impl FnMut(bool)) {
        match self {
            Self::Not(p) => p.each_clock(f),
            Self::And(ps) | Self::Or(ps) => ps.iter().for_each(|p| p.each_clock(f)),
            Self::Cmp(_, a, b) => {
                for side in [a, b] {
                    if let Operand::Clock { day, .. } = side {
                        f(*day);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn fields(&self) -> Vec<&F> {
        let mut out = Vec::new();
        self.each_field(&mut |f| out.push(f));
        out
    }

    fn each_field<'a>(&'a self, f: &mut impl FnMut(&'a F)) {
        match self {
            Self::Const(_) => {}
            Self::Not(p) => p.each_field(f),
            Self::And(ps) | Self::Or(ps) => ps.iter().for_each(|p| p.each_field(f)),
            Self::Cmp(_, a, b) => {
                for side in [a, b] {
                    if let Operand::Field(field) | Operand::Len(field) = side {
                        f(field);
                    }
                }
            }
            Self::Has(field, _)
            | Self::Like(field, _)
            | Self::Text(field, _)
            | Self::Present(field) => {
                f(field);
            }
        }
    }

    /// Its conjuncts: the predicates that all hold when it does, `And`s flattened.
    pub fn conjuncts(&self) -> Vec<&Self> {
        match self {
            Self::And(ps) => ps.iter().flat_map(Self::conjuncts).collect(),
            Self::Const(true) => Vec::new(),
            other => vec![other],
        }
    }

    /// Whether it holds of an item whose field `f` holds `value(f)`.
    ///
    /// # Errors
    ///
    /// The first error `value` returns; a comparison of values of different kinds, or of text by
    /// order, as a message.
    pub fn eval<E: From<String>>(
        &self,
        value: &mut impl FnMut(&F) -> Result<Lit, E>,
    ) -> Result<bool, E> {
        Ok(match self {
            Self::Const(b) => *b,
            Self::Not(p) => !p.eval(value)?,
            Self::And(ps) => {
                for p in ps {
                    if !p.eval(value)? {
                        return Ok(false);
                    }
                }
                true
            }
            Self::Or(ps) => {
                for p in ps {
                    if p.eval(value)? {
                        return Ok(true);
                    }
                }
                false
            }
            Self::Cmp(op, a, b) => {
                let (a, b) = (a.value(value)?, b.value(value)?);
                a.compare(*op, &b)
                    .ok_or_else(|| E::from(format!("cannot compare {a:?} with {b:?}")))?
            }
            Self::Has(f, wanted) => match value(f)? {
                Lit::Set(members) => {
                    let wanted = wanted.member_text();
                    members.iter().any(|m| m.member_text() == wanted)
                }
                Lit::Absent => false,
                one => one.compare(CmpOp::Eq, wanted).unwrap_or(false),
            },
            Self::Like(f, pattern) => match value(f)? {
                Lit::Set(members) => members.iter().any(|m| {
                    m.as_text()
                        .is_some_and(|t| crate::identifier::matches(pattern, t))
                }),
                one => one
                    .as_text()
                    .is_some_and(|t| crate::identifier::matches(pattern, t)),
            },
            Self::Text(f, needle) => {
                // Free text matches anywhere inside a text, without regard to case.
                let one = value(f)?;
                one.as_text()
                    .is_some_and(|t| t.to_lowercase().contains(&needle.to_lowercase()))
            }
            Self::Present(f) => match value(f)? {
                Lit::Absent => false,
                Lit::Set(members) => !members.is_empty(),
                _ => true,
            },
        })
    }
}

impl<F> Operand<F> {
    fn resolve<G>(&self, map: &mut impl FnMut(&F) -> Option<G>) -> Option<Operand<G>> {
        Some(match self {
            Self::Field(f) => Operand::Field(map(f)?),
            Self::Lit(l) => Operand::Lit(l.clone()),
            Self::Len(f) => Operand::Len(map(f)?),
            Self::Arg(n) => Operand::Arg(*n),
            Self::Clock { day, offset } => Operand::Clock {
                day: *day,
                offset: *offset,
            },
            Self::Viewer => Operand::Viewer,
        })
    }

    fn viewer(&self, who: &str) -> Self
    where
        F: Clone,
    {
        match self {
            Self::Viewer => Self::Lit(Lit::Principal(who.to_owned())),
            other => other.clone(),
        }
    }

    fn at(&self, now: i64, zone: i64) -> Self
    where
        F: Clone,
    {
        match self {
            Self::Clock { day: true, offset } => {
                let days = now.saturating_add(zone).div_euclid(crate::calendar::DAY)
                    + offset.div_euclid(crate::calendar::DAY);
                Self::Lit(Lit::Date(crate::calendar::day_text(days)))
            }
            Self::Clock { day: false, offset } => Self::Lit(Lit::Time(crate::calendar::time_text(
                now.saturating_add(*offset),
            ))),
            other => other.clone(),
        }
    }

    fn bind(&self, args: &[Lit]) -> Option<Self>
    where
        F: Clone,
    {
        Some(match self {
            Self::Arg(n) => Self::Lit(args.get(usize::try_from(*n).ok()?)?.clone()),
            other => other.clone(),
        })
    }

    fn value<E>(&self, value: &mut impl FnMut(&F) -> Result<Lit, E>) -> Result<Lit, E> {
        match self {
            Self::Field(f) => value(f),
            Self::Lit(l) => Ok(l.clone()),
            Self::Len(f) => Ok(Lit::Int(match value(f)? {
                Lit::Set(members) => i64::try_from(members.len()).unwrap_or(i64::MAX),
                Lit::Int(n) => n,
                Lit::Absent => 0,
                other => other.as_text().map_or(0, |t| {
                    i64::try_from(crate::text::len(t)).unwrap_or(i64::MAX)
                }),
            })),
            // Bound before it is asked ([`Pred::bind`], [`Pred::at`], [`Pred::viewer`]): one that is
            // not holds nothing.
            Self::Arg(_) | Self::Clock { .. } | Self::Viewer => Ok(Lit::Absent),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CmpOp, Lit, Operand, Pred};

    fn field(name: &str) -> Operand<&str> {
        Operand::Field(name)
    }

    fn eval(pred: &Pred<&str>, item: &[(&str, Lit)]) -> Result<bool, String> {
        pred.eval(&mut |f: &&str| {
            item.iter()
                .find(|(name, _)| name == f)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("no {f}"))
        })
    }

    #[test]
    fn a_predicate_compares_an_items_fields_with_literals() {
        let open_and_voted = Pred::And(vec![
            Pred::Not(Box::new(Pred::Cmp(
                CmpOp::Eq,
                field("done"),
                Operand::Lit(Lit::Bool(true)),
            ))),
            Pred::Cmp(CmpOp::Ge, field("votes"), Operand::Lit(Lit::Int(2))),
        ]);
        let item = |done, votes| [("done", Lit::Bool(done)), ("votes", Lit::Int(votes))];
        assert_eq!(eval(&open_and_voted, &item(false, 2)), Ok(true));
        assert_eq!(eval(&open_and_voted, &item(false, 1)), Ok(false));
        assert_eq!(eval(&open_and_voted, &item(true, 9)), Ok(false));
        assert_eq!(open_and_voted.fields(), [&"done", &"votes"]);

        let titled = Pred::Or(vec![
            Pred::Const(false),
            Pred::Cmp(
                CmpOp::Ne,
                field("title"),
                Operand::Lit(Lit::Text("x".into())),
            ),
        ]);
        assert_eq!(eval(&titled, &[("title", Lit::Text("y".into()))]), Ok(true));
        assert_eq!(
            eval(&titled, &[("title", Lit::Text("x".into()))]),
            Ok(false)
        );
    }

    #[test]
    fn text_by_order_or_values_of_two_kinds_do_not_compare() {
        let ordered = Pred::Cmp(
            CmpOp::Lt,
            field("title"),
            Operand::Lit(Lit::Text("m".into())),
        );
        assert!(eval(&ordered, &[("title", Lit::Text("a".into()))]).is_err());
        let mixed = Pred::Cmp(CmpOp::Eq, field("done"), Operand::Lit(Lit::Int(1)));
        assert!(eval(&mixed, &[("done", Lit::Bool(true))]).is_err());
    }

    #[test]
    fn resolving_a_field_that_does_not_resolve_resolves_nothing() {
        let pred = Pred::Cmp(CmpOp::Eq, field("a"), field("b"));
        assert_eq!(pred.resolve(&mut |f: &&str| (*f == "a").then_some(1)), None);
        assert_eq!(
            pred.resolve(&mut |f: &&str| Some(f.len())),
            Some(Pred::Cmp(CmpOp::Eq, Operand::Field(1), Operand::Field(1)))
        );
    }
}
