//! Stream the existing v1 wire into private derived rows. Every row has a read
//! budget before deserialization, including malformed strings/native paths.
use super::storage::{Builder, Row, Sequence, Store};
use super::*;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{cell::Cell, marker::PhantomData, rc::Rc};
const ROW_BYTES: usize = 1024 * 1024;
type Budget = Rc<Cell<Option<usize>>>;
struct Scope {
    budget: Budget,
    previous: Option<usize>,
}
impl Scope {
    fn enter(budget: &Budget, limit: Option<usize>) -> Self {
        Self {
            budget: budget.clone(),
            previous: budget.replace(limit),
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.budget.set(self.previous);
    }
}
pub(super) struct Line<'a, R: BufRead> {
    reader: &'a mut R,
    budget: Budget,
    pub(super) bytes: u64,
    complete: bool,
    maximum: u64,
    #[cfg(test)]
    pub(super) scanned_bytes: u64,
}
impl<'a, R: BufRead> Line<'a, R> {
    pub(super) fn new(reader: &'a mut R) -> Self {
        Self {
            reader,
            budget: Rc::new(Cell::new(Some(ROW_BYTES))),
            bytes: 0,
            complete: false,
            maximum: MAX as u64,
            #[cfg(test)]
            scanned_bytes: 0,
        }
    }
    pub(super) fn segmented(reader: &'a mut R) -> Self {
        let mut line = Self::new(reader);
        line.maximum = segmented::TOTAL;
        line
    }
    pub(super) fn parse(&mut self, store: Arc<Store>) -> io::Result<Plan> {
        let budget = self.budget.clone();
        let mut parser = serde_json::Deserializer::from_reader(&mut *self);
        let plan = PlanSeed { store, budget }
            .deserialize(&mut parser)
            .map_err(io::Error::other)?;
        parser.end().map_err(io::Error::other)?;
        if !self.complete {
            return Err(changed("Home adoption journal is incomplete"));
        }
        Ok(plan)
    }
}
impl<R: BufRead> Read for Line<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.complete {
            return Ok(0);
        }
        let bytes = self.reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        if bytes[0] == b'\n' {
            self.reader.consume(1);
            self.bytes += 1;
            self.complete = true;
            return Ok(0);
        }
        let limit = self.budget.get().unwrap_or(usize::MAX);
        if limit == 0 {
            return Err(changed("Home adoption metadata row exceeds bounds"));
        }
        // serde_json's reader requests individual bytes. Scanning the entire
        // buffered suffix each time makes a long Plan quadratic in the buffer
        // window; only bytes this read can consume need newline inspection.
        let available = bytes.len().min(output.len()).min(limit);
        let count = bytes[..available]
            .iter()
            .position(|b| {
                #[cfg(test)]
                {
                    self.scanned_bytes += 1;
                }
                *b == b'\n'
            })
            .unwrap_or(available);
        let length = self
            .bytes
            .checked_add(count as u64)
            .filter(|n| *n <= self.maximum)
            .ok_or_else(|| changed("Home adoption plan exceeds bounds"))?;
        output[..count].copy_from_slice(&bytes[..count]);
        self.reader.consume(count);
        self.bytes = length;
        if self.budget.get().is_some() {
            self.budget.set(Some(limit - count));
        }
        Ok(count)
    }
}
pub(super) trait Replay: Row {
    fn read<'de, D: serde::Deserializer<'de>>(
        store: Arc<Store>,
        budget: Budget,
        parser: D,
    ) -> Result<Self, D::Error>;
}
impl Replay for NativePath {
    fn read<'de, D: serde::Deserializer<'de>>(
        _: Arc<Store>,
        _: Budget,
        parser: D,
    ) -> Result<Self, D::Error> {
        Self::deserialize(parser)
    }
}
impl Replay for structure::Item {
    fn read<'de, D: serde::Deserializer<'de>>(
        _: Arc<Store>,
        _: Budget,
        parser: D,
    ) -> Result<Self, D::Error> {
        Self::deserialize(parser)
    }
}
struct RowSeed<T: Replay> {
    store: Arc<Store>,
    budget: Budget,
    _row: PhantomData<T>,
}
impl<'de, T: Replay> DeserializeSeed<'de> for RowSeed<T> {
    type Value = T;
    fn deserialize<D: serde::Deserializer<'de>>(self, parser: D) -> Result<T, D::Error> {
        let _scope = Scope::enter(&self.budget, Some(ROW_BYTES));
        T::read(self.store, self.budget, parser)
    }
}
struct SequenceSeed<T: Replay> {
    store: Arc<Store>,
    budget: Budget,
    _row: PhantomData<T>,
}
impl<'de, T: Replay> DeserializeSeed<'de> for SequenceSeed<T> {
    type Value = Sequence<T>;
    fn deserialize<D: serde::Deserializer<'de>>(self, parser: D) -> Result<Self::Value, D::Error> {
        let _scope = Scope::enter(&self.budget, None);
        parser.deserialize_seq(self)
    }
}
impl<'de, T: Replay> Visitor<'de> for SequenceSeed<T> {
    type Value = Sequence<T>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded retained metadata sequence")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
        let mut rows: Builder<T> = self.store.builder();
        while let Some(row) = values.next_element_seed(RowSeed {
            store: self.store.clone(),
            budget: self.budget.clone(),
            _row: PhantomData,
        })? {
            rows.push(row).map_err(serde::de::Error::custom)?;
        }
        Ok(rows.finish())
    }
}
struct OptionalSequence<T: Replay>(SequenceSeed<T>);
impl<'de, T: Replay> DeserializeSeed<'de> for OptionalSequence<T> {
    type Value = Option<Sequence<T>>;
    fn deserialize<D: serde::Deserializer<'de>>(self, parser: D) -> Result<Self::Value, D::Error> {
        parser.deserialize_option(self)
    }
}
impl<'de, T: Replay> Visitor<'de> for OptionalSequence<T> {
    type Value = Option<Sequence<T>>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("optional retained metadata sequence")
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_some<D: serde::Deserializer<'de>>(self, parser: D) -> Result<Self::Value, D::Error> {
        self.0.deserialize(parser).map(Some)
    }
}
impl Replay for Entry {
    fn read<'de, D: serde::Deserializer<'de>>(
        store: Arc<Store>,
        budget: Budget,
        parser: D,
    ) -> Result<Self, D::Error> {
        parser.deserialize_map(EntrySeed { store, budget })
    }
}
struct EntrySeed {
    store: Arc<Store>,
    budget: Budget,
}
impl<'de> Visitor<'de> for EntrySeed {
    type Value = Entry;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("v1 adoption entry")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entry, A::Error> {
        let (mut path, mut fact, mut items, mut parent_mode, mut kind, mut seed, mut structure) =
            (None, None, None, None, None, None, None);
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "path" if path.is_none() => path = Some(map.next_value()?),
                "fact" if fact.is_none() => fact = Some(map.next_value()?),
                "items" if items.is_none() => {
                    items = Some(map.next_value_seed(SequenceSeed {
                        store: self.store.clone(),
                        budget: self.budget.clone(),
                        _row: PhantomData,
                    })?)
                }
                "parent_mode" if parent_mode.is_none() => parent_mode = Some(map.next_value()?),
                "kind" if kind.is_none() => kind = Some(map.next_value()?),
                "seed" if seed.is_none() => {
                    seed = Some(map.next_value::<Option<seeds::Witness>>()?)
                }
                "structure" if structure.is_none() => {
                    structure = Some(map.next_value_seed(OptionalSequence(SequenceSeed {
                        store: self.store.clone(),
                        budget: self.budget.clone(),
                        _row: PhantomData,
                    }))?)
                }
                _ => {
                    return Err(serde::de::Error::custom(
                        "Unknown or duplicate v1 adoption entry field",
                    ));
                }
            }
        }
        Ok(Entry {
            path: path.ok_or_else(|| serde::de::Error::missing_field("path"))?,
            fact: fact.ok_or_else(|| serde::de::Error::missing_field("fact"))?,
            items: items.ok_or_else(|| serde::de::Error::missing_field("items"))?,
            parent_mode: parent_mode
                .ok_or_else(|| serde::de::Error::missing_field("parent_mode"))?,
            kind: kind.ok_or_else(|| serde::de::Error::missing_field("kind"))?,
            seed: seed.unwrap_or(None),
            structure: structure.unwrap_or(None),
        })
    }
}
struct PlanSeed {
    store: Arc<Store>,
    budget: Budget,
}
impl<'de> DeserializeSeed<'de> for PlanSeed {
    type Value = Plan;
    fn deserialize<D: serde::Deserializer<'de>>(self, parser: D) -> Result<Plan, D::Error> {
        parser.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for PlanSeed {
    type Value = Plan;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("v1 adoption plan")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Plan, A::Error> {
        let (
            mut version,
            mut root_acl,
            mut applications,
            mut quarantine,
            mut home,
            mut source_items,
            mut entries,
        ) = (None, None, None, None, None, None, None);
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" if version.is_none() => version = Some(map.next_value()?),
                "root_acl" if root_acl.is_none() => {
                    root_acl = Some(map.next_value::<Option<root_acl::Intent>>()?)
                }
                "applications" if applications.is_none() => applications = Some(map.next_value()?),
                "quarantine" if quarantine.is_none() => quarantine = Some(map.next_value()?),
                "home" if home.is_none() => home = Some(map.next_value()?),
                "source_items" if source_items.is_none() => {
                    source_items = Some(map.next_value_seed(SequenceSeed {
                        store: self.store.clone(),
                        budget: self.budget.clone(),
                        _row: PhantomData,
                    })?)
                }
                "entries" if entries.is_none() => {
                    entries = Some(map.next_value_seed(SequenceSeed {
                        store: self.store.clone(),
                        budget: self.budget.clone(),
                        _row: PhantomData,
                    })?)
                }
                _ => {
                    return Err(serde::de::Error::custom(
                        "Unknown or duplicate v1 adoption plan field",
                    ));
                }
            }
        }
        Ok(Plan {
            version: version.ok_or_else(|| serde::de::Error::missing_field("version"))?,
            root_acl: root_acl.unwrap_or(None),
            applications: applications.unwrap_or(false),
            quarantine: quarantine.ok_or_else(|| serde::de::Error::missing_field("quarantine"))?,
            home: home.ok_or_else(|| serde::de::Error::missing_field("home"))?,
            source_items: source_items
                .ok_or_else(|| serde::de::Error::missing_field("source_items"))?,
            entries: entries.ok_or_else(|| serde::de::Error::missing_field("entries"))?,
        })
    }
}
