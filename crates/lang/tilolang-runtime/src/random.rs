//! Imports over draws: `random.int(lo, hi)`, `random.float()`, `random.choice(xs)`,
//! `random.shuffle(xs)` and `random.id()`.
//!
//! A draw is made where the action runs, on the device invoking it, from the system's
//! CSPRNG, and the number it drew is kept in the action's journal: a run replayed
//! reads the numbers it drew the first time (effects.md, "Modules"). All five
//! are one primitive, a number in `0..upto` the draw is uniform over; a choice, a shuffle, a
//! float and an id are arithmetic on those numbers, so each keeps a journal entry per number it
//! drew rather than one per call.

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_f64, ret_i64, trap};
use crate::engine::{HostFail, Wasm};
use crate::run::JournalEntry;
use crate::value::Value;

/// A `random.float()` is one draw of this many bits out of `2^53`, which is every integer an
/// `f64` holds exactly: the number it makes is drawn / `2^53`, in `[0, 1)`.
const FLOAT_BITS: i64 = 1 << 53;

/// An id is four draws of this many bits, written as eight hex digits each: 32 hex digits,
/// 128 bits, with no two of them more likely than any other.
const ID_BITS: i64 = 1 << 32;

impl Core {
    pub(crate) fn random_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::RandomInt => self.random_int(a),
            Imp::RandomFloat => float(self.draw(FLOAT_BITS)?),
            Imp::RandomChoice => self.random_choice(a),
            Imp::RandomShuffle => self.random_shuffle(a),
            Imp::RandomId => {
                let one = self.draw(ID_BITS)?;
                let two = self.draw(ID_BITS)?;
                let three = self.draw(ID_BITS)?;
                let four = self.draw(ID_BITS)?;
                self.ret_h(Value::Text(format!(
                    "{one:08x}{two:08x}{three:08x}{four:08x}"
                )))
            }
            _ => trap("not a draw"),
        }
    }

    fn random_int(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let (lo, hi) = (a.long(0)?, a.long(1)?);
        // The numbers it may draw, as a count: `random.int(1, 6)` has six of them.
        let span = i128::from(hi)
            .saturating_sub(i128::from(lo))
            .saturating_add(1);
        let Ok(span) = i64::try_from(span) else {
            return trap(format!(
                "`random.int({lo}, {hi})` spans more numbers than a draw has: draw two and combine them"
            ));
        };
        if span <= 0 {
            return trap(format!(
                "`random.int({lo}, {hi})` draws from no number: the lowest is above the highest"
            ));
        }
        let Some(value) = lo.checked_add(self.draw(span)?) else {
            return trap(format!("`random.int({lo}, {hi})` drew outside its range"));
        };
        ret_i64(value)
    }

    fn random_choice(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let items = self.list_items(a.h(0)?)?;
        // A list with nothing in it draws nothing, so it records nothing and reads `.none`.
        if items.is_empty() {
            return self.ret_h(Value::Opt(None));
        }
        let at = within(self.draw(count(items.len())?)?, items.len())?;
        let Some(item) = items.get(at) else {
            return trap("`random.choice`: the item it drew is gone");
        };
        self.ret_h(Value::Opt(Some(Box::new(item.clone()))))
    }

    fn random_shuffle(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let mut items = self.list_items(a.h(0)?)?;
        // Fisher-Yates: each item is placed from the ones not placed yet, so every order is as
        // likely as every other.
        for at in (1..items.len()).rev() {
            // The places not placed yet, which are `0..=at`.
            let places = at.saturating_add(1);
            let with = within(self.draw(count(places)?)?, places)?;
            items.swap(at, with);
        }
        self.ret_h(Value::List(items))
    }

    /// A draw from `0..upto`: the number this invocation drew for the call it is making when it
    /// drew one before, answered from its journal, else one from the host's CSPRNG, which it
    /// records. Code that journals nothing (a view's) cannot draw.
    fn draw(&self, upto: i64) -> HResult<i64> {
        if upto <= 0 {
            return trap(format!("a draw from 0..{upto} has nothing to answer with"));
        }
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("a draw is only available in an action");
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Random {
                    upto: drawn,
                    value,
                } = entry
                else {
                    return trap(
                        "the run diverged from its journal: expected the entry of the draw it made",
                    );
                };
                if *drawn != upto || !(0..upto).contains(value) {
                    return trap(format!(
                        "the run diverged from its journal: it drew {value} of 0..{drawn} where it draws one of 0..{upto}"
                    ));
                }
                let value = *value;
                run.cursor = run.cursor.saturating_add(1);
                return Ok(value);
            }
        }
        let value = below(upto)?;
        self.journal_push(JournalEntry::Random { upto, value })?;
        Ok(value)
    }
}

/// `random.float()`: one draw below `2^53`, as the fraction of them it is.
fn float(drawn: i64) -> HResult<Option<Wasm>> {
    #[allow(
        clippy::cast_precision_loss,
        reason = "the draw is below 2^53, which every f64 holds exactly"
    )]
    let fraction = drawn as f64 / FLOAT_BITS as f64;
    ret_f64(fraction)
}

/// How many a list of `len` is, as the bound of a draw.
fn count(len: usize) -> HResult<i64> {
    i64::try_from(len).or_else(|_| trap(format!("a list of {len} is too long to draw from")))
}

/// The draw as the place in a list of `len` it names.
fn within(drawn: i64, len: usize) -> HResult<usize> {
    usize::try_from(drawn).or_else(|_| trap(format!("the draw {drawn} names no place in {len}")))
}

/// A number in `0..upto`, uniform, from the system's CSPRNG: values at or above the largest
/// multiple of `upto` are drawn again, so no number is likelier than another.
fn below(upto: i64) -> HResult<i64> {
    let Ok(bound) = u64::try_from(upto) else {
        return trap(format!("a draw from 0..{upto} has nothing to answer with"));
    };
    let limit = u64::MAX.saturating_sub(u64::MAX.wrapping_rem(bound));
    loop {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes)
            .map_err(|e| HostFail::Trap(format!("no randomness from the system: {e}")))?;
        let drawn = u64::from_le_bytes(bytes);
        if drawn < limit {
            // Below `limit`, which is no more than the numbers an `i64` holds.
            let Some(drawn) = drawn.checked_rem(bound) else {
                return trap("a draw with nothing to divide it by");
            };
            let Ok(drawn) = i64::try_from(drawn) else {
                return trap("a draw below its bound");
            };
            return Ok(drawn);
        }
    }
}
