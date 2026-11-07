//! The columns that read the clock (search.md, "Who indexes"). A named filter comparing a field
//! with the time it is asked at ("Inbox": `snoozed_until <= now`, "Today": `due <= today`) is a
//! column like any other, written at the clock the index is derived at ([`super::Clock`]): its
//! run is the items that hold of it, and its counts answer how many, so a query names it and
//! reads one run and one count rather than checking its tests item by item.
//!
//! The clock runs on without the record changing, so the index records the instant it wrote such
//! a type's columns at ([`META_CLOCK`]) and, when it is asked at a later one, writes again the
//! items whose clock-read field the clock passed between the two: those, and only those, can
//! have come to hold of the filter or stopped holding of it, so what it holds and how many it
//! holds stay exact without reading what did not move. A column that reads the person's day
//! changes with their zone as well, so a zone other than the one it was written in writes them
//! all again.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::columns;
use super::derive::{Columns, field_value, item_path, placed_in};
use super::{Clock, Index, IndexError, META_CLOCK, Snapshot, TYPE, Writer};
use crate::schema::{ColumnSchema, TypeSchema, lookup};
use tilolang::pred::{Operand, Pred};

impl Index {
    /// The instant the columns of `ty` that read the clock were written at, and the zone they
    /// read the person's day in, if the index wrote any.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn written_at(&self, ty: &str) -> Result<Option<(i64, i64)>, IndexError> {
        let read = (|| {
            let txn = redb::ReadableDatabase::begin_read(&*self.db)?;
            let meta = txn.open_table(super::META)?;
            let Some(value) = meta.get(format!("{META_CLOCK}{ty}").as_str())? else {
                return Ok(None);
            };
            let text = super::codec::get_text(value.value())?;
            let mut parts = text.split(' ');
            let (Some(now), Some(zone)) = (parts.next(), parts.next()) else {
                return Err(IndexError::Corrupt);
            };
            let now = now.parse().map_err(|_| IndexError::Corrupt)?;
            let zone = zone.parse().map_err(|_| IndexError::Corrupt)?;
            Ok(Some((now, zone)))
        })();
        if matches!(read, Err(IndexError::Corrupt)) {
            self.stats.corrupt();
        }
        read
    }

    /// Write again the columns of the filters of `schema` that read the clock, as `clock` has it:
    /// the items whose clock-read field it passed since the index last wrote them, and nothing
    /// else. Answers whether it wrote.
    ///
    /// # Errors
    /// The index cannot be written.
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
        if clock.now <= written && clock.zone == zone {
            return Ok(false);
        }
        let snapshot = self.snapshot()?;
        let mut paths = BTreeSet::new();
        for column in live {
            for (field, day) in clock_fields(column) {
                paths.extend(if clock.zone == zone {
                    crossed(&snapshot, schema, field, day, written, clock)?
                } else {
                    every(&snapshot, schema, field)?
                });
            }
        }
        self.rewrite(schema, clock, &paths)
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
    /// they read the person's day in, unless the index records one already.
    ///
    /// # Errors
    /// The index cannot be written.
    pub fn note_written_at(&mut self, ty: &str, clock: Clock) -> Result<(), IndexError> {
        let key = format!("{META_CLOCK}{ty}");
        if redb::ReadableTable::get(&self.tables.meta, key.as_str())?.is_some() {
            return Ok(());
        }
        self.set_written_at(ty, clock)
    }

    /// Record the instant the columns of `ty` that read the clock are written at, and the zone
    /// they read the person's day in.
    ///
    /// # Errors
    /// The index cannot be written.
    pub fn set_written_at(&mut self, ty: &str, clock: Clock) -> Result<(), IndexError> {
        let key = format!("{META_CLOCK}{ty}");
        let text = format!("{} {}", clock.now, clock.zone);
        self.tables
            .meta
            .insert(key.as_str(), super::codec::put_text(&text).as_slice())?;
        Ok(())
    }
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
    let mut out = Vec::new();
    if let Some(pred) = &column.pred {
        reads_of(pred, &mut out);
    }
    out
}

fn reads_of<'p>(pred: &'p Pred<String>, out: &mut Vec<(&'p str, bool)>) {
    match pred {
        Pred::Not(inner) => reads_of(inner, out),
        Pred::And(parts) | Pred::Or(parts) => {
            for part in parts {
                reads_of(part, out);
            }
        }
        Pred::Cmp(_, a, b) => {
            // Either side of the comparison may be the field: a filter may ask `f <= now` or
            // `now <= f`, and both are written again for the same values.
            let (side, day) = match (a, b) {
                (Operand::Field(f), Operand::Clock { day, .. })
                | (Operand::Clock { day, .. }, Operand::Field(f)) => (f.as_str(), *day),
                _ => return,
            };
            out.push((side, day));
        }
        _ => {}
    }
}
