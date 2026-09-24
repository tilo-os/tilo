//! Counted native cursor encoding; no SlabWriter actions or RLE literal Vec.
use super::{add, Result};
use crate::resource_limits::{NativeAllowance, NativeReservation, NativeResourceError};
use crate::storage::ColumnSpec;
use hexane::{ColumnCursor, ColumnData, ColumnDataIter, Packable};
use std::{borrow::Cow, mem::size_of, ops::Range};

pub(crate) struct Sink<'a> {
    pub(crate) output: Option<&'a mut Vec<u8>>,
    pub(crate) len: usize,
    pub(crate) limit: usize,
}
impl Sink<'_> {
    fn advance(&mut self, bytes: usize) -> Result {
        let end = add(self.len, bytes)?;
        if end > self.limit {
            return Err(NativeResourceError::WorkBytes);
        }
        if let Some(output) = &self.output {
            if add(output.len(), bytes)? > output.capacity() {
                return Err(NativeResourceError::Unaccounted);
            }
        }
        self.len = end;
        Ok(())
    }
    pub(crate) fn bytes(&mut self, value: &[u8]) -> Result {
        self.advance(value.len())?;
        if let Some(output) = &mut self.output {
            output.extend_from_slice(value);
        }
        Ok(())
    }
    pub(crate) fn pack<P: Packable + ?Sized>(&mut self, value: &P) -> Result {
        let width = P::width(value);
        self.advance(width)?;
        if let Some(output) = &mut self.output {
            let before = output.len();
            P::pack(value, output);
            if output.len() - before != width {
                return Err(NativeResourceError::Unaccounted);
            }
        }
        Ok(())
    }
}

// Private whitelist: all these native Packable implementations emit directly
// into the supplied Vec and their declared width is exact. Their cursor/slab
// weights are POD and borrowed strings/raw bytes never become owned values.
pub(crate) trait NativeCursor: ColumnCursor {
    type NativeState: Default;
    fn item(
        state: &mut Self::NativeState,
        value: Option<Cow<'_, Self::Item>>,
        sink: &mut Sink<'_>,
    ) -> Result;
    fn finish(_state: &mut Self::NativeState, _sink: &mut Sink<'_>) -> Result {
        Ok(())
    }
}
fn literal<P: Packable + ?Sized>(value: Option<&P>, sink: &mut Sink<'_>) -> Result {
    if let Some(value) = value {
        sink.pack(&-1i64)?;
        sink.pack(value)
    } else {
        sink.bytes(&[0, 1])
    }
}
macro_rules! rle {
    ($($cursor:ty),* $(,)?) => {$(impl NativeCursor for $cursor {
        type NativeState = ();
        fn item(_: &mut (), value: Option<Cow<'_, Self::Item>>, sink: &mut Sink<'_>) -> Result {
            literal(value.as_deref(), sink)
        }
    })*};
}
rle!(
    hexane::UIntCursor,
    hexane::IntCursor,
    hexane::StrCursor,
    crate::op_set2::ActorCursor,
    crate::op_set2::types::ActionCursor,
    crate::op_set2::MetaCursor
);
impl NativeCursor for hexane::DeltaCursor {
    type NativeState = i64;
    fn item(previous: &mut i64, value: Option<Cow<'_, i64>>, sink: &mut Sink<'_>) -> Result {
        match value {
            None => literal::<i64>(None, sink),
            Some(value) => {
                let delta = value
                    .checked_sub(*previous)
                    .ok_or(NativeResourceError::Unaccounted)?;
                *previous = *value;
                literal(Some(&delta), sink)
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct BooleanState {
    current: bool,
    count: u64,
}
impl NativeCursor for hexane::BooleanCursor {
    type NativeState = BooleanState;
    fn item(state: &mut BooleanState, value: Option<Cow<'_, bool>>, sink: &mut Sink<'_>) -> Result {
        let value = value.as_deref().copied().unwrap_or(false);
        if value != state.current {
            sink.pack(&state.count)?;
            state.current = value;
            state.count = 0;
        }
        state.count = state
            .count
            .checked_add(1)
            .ok_or(NativeResourceError::WorkBytes)?;
        Ok(())
    }
    fn finish(state: &mut BooleanState, sink: &mut Sink<'_>) -> Result {
        if state.count != 0 {
            sink.pack(&state.count)?;
        }
        Ok(())
    }
}
impl NativeCursor for hexane::RawCursor {
    type NativeState = ();
    fn item(_: &mut (), value: Option<Cow<'_, [u8]>>, sink: &mut Sink<'_>) -> Result {
        match value {
            Some(value) => sink.bytes(&value),
            None => Err(NativeResourceError::Unaccounted),
        }
    }
}

pub(crate) struct Column {
    pub(crate) spec: ColumnSpec,
    pub(crate) range: Range<usize>,
}
pub(crate) struct Columns {
    pub(crate) columns: Vec<Column>,
    pub(crate) change_count: usize,
    pub(crate) bytes: usize,
    _reservation: NativeReservation,
}
impl Columns {
    pub(crate) fn new(allowance: &NativeAllowance) -> Result<Self> {
        const COUNT: usize = 25; // 9 change columns + 16 native op columns.
        let reservation = allowance.reserve(COUNT * size_of::<Column>())?;
        let mut columns = Vec::new();
        columns
            .try_reserve_exact(COUNT)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if columns.capacity() * size_of::<Column>() > reservation.bytes() {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(Self {
            columns,
            change_count: 0,
            bytes: 0,
            _reservation: reservation,
        })
    }
}
pub(crate) struct Pass<'a, 'b> {
    pub(crate) allowance: &'a NativeAllowance,
    pub(crate) sink: Sink<'b>,
    pub(crate) columns: &'b mut Columns,
    pub(crate) writing: bool,
    pub(crate) index: usize,
}
impl Pass<'_, '_> {
    fn record(&mut self, spec: ColumnSpec, start: usize) -> Result {
        if self.sink.len == start {
            return Ok(());
        }
        if self.writing {
            let old = self
                .columns
                .columns
                .get(self.index)
                .ok_or(NativeResourceError::Unaccounted)?;
            if old.spec != spec || old.range != (start..self.sink.len) {
                return Err(NativeResourceError::Unaccounted);
            }
        } else {
            if self.columns.columns.len() == self.columns.columns.capacity() {
                return Err(NativeResourceError::Unaccounted);
            }
            self.columns.columns.push(Column {
                spec,
                range: start..self.sink.len,
            });
        }
        self.index += 1;
        Ok(())
    }
    pub(crate) fn values<'v, C, I, F>(&mut self, spec: ColumnSpec, create: F) -> Result
    where
        C: NativeCursor,
        C::Item: 'v,
        I: Iterator<Item = Option<Cow<'v, C::Item>>>,
        F: FnOnce() -> I,
    {
        self.values_maybe_empty::<C, I, F>(spec, create, false)
    }
    fn values_maybe_empty<'v, C, I, F>(
        &mut self,
        spec: ColumnSpec,
        create: F,
        omit_empty: bool,
    ) -> Result
    where
        C: NativeCursor,
        C::Item: 'v,
        I: Iterator<Item = Option<Cow<'v, C::Item>>>,
        F: FnOnce() -> I,
    {
        let start = self.sink.len;
        let _credit = self.allowance.reserve(size_of::<(I, C::NativeState)>())?;
        let mut work = Box::new((create(), C::NativeState::default()));
        let mut empty = true;
        while let Some(value) = work.0.next() {
            empty &= C::is_empty(value.clone());
            C::item(&mut work.1, value, &mut self.sink)?;
        }
        <C as NativeCursor>::finish(&mut work.1, &mut self.sink)?;
        if omit_empty && empty {
            self.sink.len = start;
            if let Some(output) = &mut self.sink.output {
                output.truncate(start);
            }
        }
        self.record(spec, start)
    }
    pub(crate) fn column<C: NativeCursor>(
        &mut self,
        spec: ColumnSpec,
        value: &ColumnData<C>,
        required: bool,
    ) -> Result {
        if value.len() == 0 {
            return Ok(());
        }
        // The immutable count pass already proved omission. Do not emit and
        // then truncate bytes that were deliberately excluded from capacity.
        if self.writing && self.columns.columns.get(self.index).map(|c| c.spec) != Some(spec) {
            return Ok(());
        }
        self.values_maybe_empty::<C, ColumnDataIter<'_, C>, _>(
            spec,
            || value.iter_at_start(),
            !required,
        )
    }
    pub(crate) fn raw(&mut self, spec: ColumnSpec, bytes: &[u8]) -> Result {
        let start = self.sink.len;
        self.sink.bytes(bytes)?;
        self.record(spec, start)
    }
}
