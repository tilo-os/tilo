//! The time the store reads: what a column that compares a field with `now` or `today` is
//! written at ([`crate::index::clock`]), and what a later read writes again for.
//!
//! The host names both, through the closures [`ClockSource`] holds: tests and captures fix
//! them, a window reads the system's.

use std::rc::Rc;

use super::{TiloStore, backend};
use tilolang_runtime::ids::TypeId;
use tilolang_runtime::query::{ClockField, NextChange};
use tilolang_runtime::store::StoreError;

/// How the host names the time and the person's zone: what a column reading the clock is written
/// at, read at the moment the index is asked ([`crate::index::clock`]).
#[derive(Clone)]
pub(crate) struct ClockSource {
    pub(super) now: Rc<dyn Fn() -> String>,
    pub(super) zone: Rc<dyn Fn() -> i64>,
}

impl Default for ClockSource {
    fn default() -> Self {
        Self {
            now: Rc::new(crate::host::system_time),
            zone: Rc::new(|| 0),
        }
    }
}

impl ClockSource {
    /// The clock as it reads now: the instant, to the minute, and the zone.
    pub(super) fn read(&self) -> crate::index::Clock {
        crate::index::Clock::at(&(self.now)(), (self.zone)())
    }
}

impl TiloStore {
    /// The time the columns that read the clock are written at, as the host names it
    /// ([`crate::index::Clock`]): the instant, to the minute, and the person's zone.
    #[must_use]
    pub fn clock(&self) -> crate::index::Clock {
        self.held().clock.read()
    }

    /// How the store reads `now` (`YYYY-MM-DDTHH:MM:SSZ`), from the host: what a list read later
    /// writes again for the instants the clock passed ([`Self::refresh`]).
    pub fn set_now(&self, now: impl Fn() -> String + 'static) {
        self.mutate().clock.now = Rc::new(now);
        self.exact_at_the_clock();
    }

    /// Where the person is, in seconds east of UTC, from the host: their day, which `today`
    /// reads.
    pub fn set_zone(&self, zone: impl Fn() -> i64 + 'static) {
        self.mutate().clock.zone = Rc::new(zone);
        self.exact_at_the_clock();
    }

    /// The clock the host named is the one the columns that read it answer at from here on:
    /// write again what it passed since the index last wrote them, so a read after it answers
    /// from the columns rather than writing them as it goes.
    fn exact_at_the_clock(&self) {
        let types: Vec<String> = {
            let inner = self.held();
            inner
                .apps
                .values()
                .filter(|held| held.ready())
                .flat_map(|held| held.index.recorded().unwrap_or_default())
                .collect()
        };
        for ty in types {
            self.refresh(&ty);
        }
    }

    /// The instant the items of `ty` next come to hold of, or stop holding of, a filter that
    /// reads the clock (`fields` naming the fields it compares it with): the value the index,
    /// and the store's writes the index has not taken in, keep that comes closest after it, so
    /// the view that asked wakes there rather than at every minute. A package whose index is
    /// still being built cannot say ([`NextChange::Unknown`]).
    pub(super) fn next_change(
        &self,
        ty: &TypeId,
        fields: &[ClockField],
    ) -> Result<NextChange, StoreError> {
        let Some(schema) = crate::schema::lookup(ty) else {
            return Ok(NextChange::Unknown);
        };
        let clock = self.clock();
        let inside = self.held();
        match inside.apps.get(&schema.app).filter(|held| held.ready()) {
            Some(held) => held
                .index
                .next_change(&schema.id, fields, clock, &inside.overlay)
                .map_err(backend),
            None => Ok(NextChange::Unknown),
        }
    }

    /// Write again what the clock changed in the index of `ty`'s package: the filters that read
    /// the clock, for the items whose clock-read field it passed since the index last wrote them.
    pub(super) fn refresh(&self, ty: &TypeId) {
        let (_schemas, _scope) = self.scoped();
        let Some(schema) = crate::schema::lookup(ty) else {
            return;
        };
        let clock = self.clock();
        let wrote = {
            let mut inner = self.mutate();
            match inner
                .apps
                .get(&schema.app)
                .filter(|held| held.ready())
                .map(|held| held.index.refresh(schema, clock))
            {
                None | Some(Ok(false)) => return,
                Some(Ok(true)) => true,
                Some(Err(error)) => {
                    tracing::warn!(app = schema.app, %error, "the clock was not indexed");
                    inner.rebuild(&schema.app);
                    return;
                }
            }
        };
        if !wrote {
            return;
        }
        let mut inner = self.mutate();
        inner.changed();
        match inner
            .apps
            .get(&schema.app)
            .map(|held| held.index.snapshot())
        {
            Some(Ok(snapshot)) if !snapshot.stats().is_corrupt() => {
                if let Some(held) = inner.apps.get_mut(&schema.app) {
                    held.snapshot = snapshot;
                }
            }
            _ => inner.rebuild(&schema.app),
        }
    }
}
