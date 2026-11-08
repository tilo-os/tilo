//! The columns that read the clock (search.md, "Who indexes"). A named filter comparing a field
//! with the time it is asked at ("Inbox": `snoozed_until <= now`, "Today": `due <= today`) is a
//! column like any other, written at the clock the index is derived at ([`super::Clock`]): its
//! run is the items that hold of it, and its counts answer how many, so a query names it and
//! reads one run and one count rather than checking its tests item by item.
//!
//! The clock runs on without the record changing, so the index records the instant it wrote such
//! a type's columns at ([`META_CLOCK`]) and, when it is asked at another one, writes again the
//! items whose clock-read field the clock passed between the two: those, and only those, can
//! have come to hold of the filter or stopped holding of it, so what it holds and how many it
//! holds stay exact without reading what did not move. That holds whichever way the clock moved:
//! the index answers the instant it is asked at, so an instant behind the one it wrote at writes
//! again too. A column that reads the person's day changes with their zone as well, so a zone
//! other than the one it was written in writes them all again.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::columns;
use super::derive::{Columns, field_value, item_path, placed_in};
use super::{Clock, Index, IndexError, META_CLOCK, Snapshot, TYPE, Writer};
use crate::schema::{ColumnSchema, TypeSchema, lookup};
use tilolang::pred::Pred;
use tilolang_runtime::query::{ClockField, NextChange};

impl Index {
    /// The instant the columns of `ty` that read the clock were written at, and the zone they
    /// read the person's day in, if the index wrote any.
    pub fn written_at(&self, ty: &str) -> Result<Option<(i64, i64)>, IndexError> {
        let read = (|| {
            let txn = redb::ReadableDatabase::begin_read(&*self.db)?;
            let meta = txn.open_table(super::META)?;
            let Some(value) = meta.get(format!("{META_CLOCK}{ty}").as_str())? else {
                return Ok(None);
            };
            let text = super::codec::get_text(value.value())?;
            let clock = recorded(&text).ok_or(IndexError::Corrupt)?;
            Ok(Some((clock.now, clock.zone)))
        })();
        if matches!(read, Err(IndexError::Corrupt)) {
            self.stats.corrupt();
        }
        read
    }

    /// The types whose columns that read the clock the index wrote, and so has to write again
    /// when the clock they are read at changes: what the host naming a clock makes exact at it
    /// ([`crate::store::TiloStore::set_now`]).
    pub fn recorded(&self) -> Result<Vec<String>, IndexError> {
        let read = (|| {
            let txn = redb::ReadableDatabase::begin_read(&*self.db)?;
            let meta = txn.open_table(super::META)?;
            let mut out = Vec::new();
            for entry in meta.range(META_CLOCK..)? {
                let (key, _) = entry?;
                let Some(ty) = key.value().strip_prefix(META_CLOCK) else {
                    break;
                };
                out.push(ty.to_owned());
            }
            Ok(out)
        })();
        if matches!(read, Err(IndexError::Corrupt)) {
            self.stats.corrupt();
        }
        read
    }

    /// Write again the columns of the filters of `schema` that read the clock, as `clock` has it:
    /// the items whose clock-read field the clock passed since the index last wrote them, and
    /// nothing else. Answers whether it wrote.
    pub fn refresh(
        self: &Arc<Self>,
        schema: &'static TypeSchema,
        clock: Clock,
    ) -> Result<bool, IndexError> {
        let live: Vec<&ColumnSchema> = schema.columns.iter().filter(|c| reads_clock(c)).collect();
        if live.is_empty() {
            return Ok(false);
        }
        let Some((written, zone)) = self.written_at(&schema.id)? else {
            // The index wrote none of them: it is derived whole, which writes them as it goes.
            return Ok(false);
        };
        let from = Clock { now: written, zone };
        if clock == from {
            return Ok(false);
        }
        let snapshot = self.snapshot()?;
        let mut paths = BTreeSet::new();
        for column in live {
            for (field, day) in clock_fields(column) {
                paths.extend(if clock.zone == zone {
                    // The items the clock passed between the instant it wrote at and this one,
                    // from the earlier of the two: a clock behind what it wrote at passed them
                    // the other way, and their answer moved with it.
                    let (there, here) = if from.now < clock.now {
                        (from, clock)
                    } else {
                        (clock, from)
                    };
                    crossed(&snapshot, schema, field, day, there.now, here)?
                } else {
                    every(&snapshot, schema, field)?
                });
            }
        }
        self.rewrite(schema, clock, &paths)
    }

    /// When what the filters of `ty` hold of next changes as the clock runs: the value of one of
    /// `fields` (each field the query compared with the clock, [`ClockField`]) that lies closest
    /// after it, so the view that asked wakes when that item comes to count, and not before.
    /// Answering `Unknown` when the index cannot place the field leaves the clock's grain to
    /// wake the view instead, and `Never` when it keeps no later value at all.
    pub fn next_change(
        self: &Arc<Self>,
        ty: &str,
        fields: &[ClockField],
        clock: Clock,
    ) -> Result<NextChange, IndexError> {
        let Some(schema) = lookup(ty) else {
            return Ok(NextChange::Unknown);
        };
        let snapshot = self.snapshot()?;
        let mut looked = false;
        let mut next: Option<i64> = None;
        for (field, _, offset) in fields.iter().filter(|(_, day, _)| !day) {
            // The value the filter compares is the field's, and the clock is ahead of now by
            // the offset: the items holding a value after it are the ones that come next.
            let Some(spec) = schema.column(field) else {
                continue;
            };
            let Some(after) = columns::clock_value(
                spec.kind,
                clock.now.saturating_add(*offset),
                false,
                clock.zone,
            ) else {
                continue;
            };
            looked = true;
            for (list, _) in snapshot.lists_from(columns::SCOPE)? {
                if !of_column(&list, &spec.name) {
                    continue;
                }
                let Some(at) = first_after(&snapshot, &list, &after)? else {
                    continue;
                };
                // A value `v` the clock passes at `now + offset` changes what the filter holds
                // of its item at `v - offset`: when the offseted clock reaches it. The clock is
                // read to the minute, so that is the minute boundary it reaches it on.
                let at = minute(at.saturating_sub(*offset));
                next = Some(next.map_or(at, |seen| seen.min(at)));
            }
        }
        if !looked {
            return Ok(NextChange::Unknown);
        }
        Ok(next.map_or(NextChange::Never, NextChange::At))
    }

    /// Write the columns of the filters of `schema` that read the clock for `paths`, and record
    /// the instant as the one they are at.
    fn rewrite(
        &self,
        schema: &'static TypeSchema,
        clock: Clock,
        paths: &BTreeSet<String>,
    ) -> Result<bool, IndexError> {
        self.write(|writer| {
            let mut columns = Columns::default();
            for path in paths {
                let Some(ty) = writer.text(path, TYPE)? else {
                    continue;
                };
                let item = lookup(&ty).unwrap_or(schema);
                let Some(scope) = placed_in(writer, path)? else {
                    continue;
                };
                let scope = Some((scope.0.as_str(), scope.1.as_str(), scope.2.as_str()));
                for column in item.columns.iter().filter(|c| reads_clock(c)) {
                    let now = columns::values(column, item, clock, &mut |field| {
                        field_value(writer, path, field)
                    })?;
                    columns.write(writer, path, item, column, scope, &now)?;
                }
            }
            writer.set_written_at(&schema.id, clock)?;
            Ok(())
        })?;
        Ok(true)
    }
}

/// The items of `schema` whose column `field` the clock passed from `from` to `clock`: the
/// values of the field the two bracket, across every collection of the workspace.
fn crossed(
    snapshot: &Arc<Snapshot>,
    schema: &TypeSchema,
    field: &str,
    day: bool,
    from: i64,
    clock: Clock,
) -> Result<Vec<String>, IndexError> {
    let Some(spec) = schema.column(field) else {
        return Ok(Vec::new());
    };
    let written = |at: i64| columns::clock_value(spec.kind, at, day, clock.zone);
    let (Some(lo), Some(hi)) = (written(from), written(clock.now)) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (list, _) in snapshot.lists_from(columns::SCOPE)? {
        if of_column(&list, &spec.name) {
            out.extend(ids_in(snapshot, &list, Some((&lo, &hi)))?);
        }
    }
    Ok(out)
}

/// The instant `at` where the clock reads it: a reading is whole minutes, so the first minute
/// at or after it.
fn minute(at: i64) -> i64 {
    at.saturating_sub(at.rem_euclid(60))
        .saturating_add(if at.rem_euclid(60) == 0 { 0 } else { 60 })
}

/// The value of the first entry of the column `list` that comes after the value `after`, as the
/// instant it names: the next moment the clock passing it changes what the column reads.
fn first_after(
    snapshot: &Arc<Snapshot>,
    list: &str,
    after: &str,
) -> Result<Option<i64>, IndexError> {
    let pages = snapshot.pages(list)?;
    let start = pages.rank(&columns::past_value(after), "").0;
    let Some((key, _)) = pages.entry(start) else {
        return Ok(None);
    };
    let value = key
        .split_once(columns::SEP)
        .map_or(key.as_str(), |(v, _)| v);
    Ok(tilolang::calendar::instant(value))
}

/// Every item of `schema` holding the column `field`, wherever it is in the workspace: what a
/// change of zone writes again.
fn every(
    snapshot: &Arc<Snapshot>,
    schema: &TypeSchema,
    field: &str,
) -> Result<Vec<String>, IndexError> {
    let Some(spec) = schema.column(field) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (list, _) in snapshot.lists_from(columns::SCOPE)? {
        if of_column(&list, &spec.name) {
            out.extend(ids_in(snapshot, &list, None)?);
        }
    }
    Ok(out)
}

/// The items of the collection column `list`, whose value stands from `range` (all of them for
/// none).
fn ids_in(
    snapshot: &Arc<Snapshot>,
    list: &str,
    range: Option<(&str, &str)>,
) -> Result<Vec<String>, IndexError> {
    let pages = snapshot.pages(list)?;
    let (start, end) = match range {
        Some((lo, hi)) => (
            pages.rank(&columns::at_value(lo), "").0,
            pages.rank(&columns::past_value(hi), "").0,
        ),
        None => (0, pages.len()),
    };
    let Some((parent, _, _)) = columns::scope_of(list) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(end.saturating_sub(start));
    for index in start..end {
        if let Some((_, id)) = pages.entry(index) {
            out.push(item_path(parent, &id));
        }
    }
    Ok(out)
}

impl Writer<'_> {
    /// Record the instant the columns of `ty` that read the clock are written at, and the zone
    /// they read the person's day in, as the earliest of it and the one recorded: the columns a
    /// write leaves behind hold of their filters from this instant on, so an instant earlier than
    /// the one recorded means a later read has to write them again from there
    /// ([`Index::refresh`]).
    pub fn note_written_at(&mut self, ty: &str, clock: Clock) -> Result<(), IndexError> {
        let key = format!("{META_CLOCK}{ty}");
        let holds = match redb::ReadableTable::get(&self.tables.meta, key.as_str())? {
            None => false,
            Some(held) => {
                let held = super::codec::get_text(held.value())?;
                recorded(&held).is_some_and(|written| written.now <= clock.now)
            }
        };
        if holds {
            return Ok(());
        }
        self.set_written_at(ty, clock)
    }

    /// Record the instant the columns of `ty` that read the clock are written at, and the zone
    /// they read the person's day in.
    pub fn set_written_at(&mut self, ty: &str, clock: Clock) -> Result<(), IndexError> {
        let key = format!("{META_CLOCK}{ty}");
        let text = format!("{} {}", clock.now, clock.zone);
        self.tables
            .meta
            .insert(key.as_str(), super::codec::put_text(&text).as_slice())?;
        Ok(())
    }
}

/// The clock `text` records ([`META_CLOCK`]): the instant and the zone it was written in.
fn recorded(text: &str) -> Option<Clock> {
    let mut parts = text.split(' ');
    let (Some(now), Some(zone)) = (parts.next(), parts.next()) else {
        return None;
    };
    Some(Clock {
        now: now.parse().ok()?,
        zone: zone.parse().ok()?,
    })
}

/// Whether the list is a collection's column of the column `column`.
fn of_column(list: &str, column: &str) -> bool {
    columns::scope_of(list).is_some_and(|(_, _, named)| named == column)
}

/// Whether the column holds whether a filter reading the clock does.
fn reads_clock(column: &ColumnSchema) -> bool {
    column.filter
        && column
            .pred
            .as_ref()
            .is_some_and(tilolang::pred::Pred::reads_clock)
}

/// The fields a filter reads the clock against, and whether it reads the day (`today`) or the
/// instant (`now`): the values that turning it over changes its answer for.
fn clock_fields(column: &ColumnSchema) -> Vec<(&str, bool)> {
    column
        .pred
        .iter()
        .flat_map(Pred::clock_fields)
        .map(|(field, day, _)| (field.as_str(), day))
        .collect()
}
