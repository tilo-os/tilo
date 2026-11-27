//! `sfdisk --json` as a typed partition table.
//!
//! Every disk tool asks `sfdisk --json <disk>` for the table's label, the disk's GPT GUID and each
//! partition's identity. Parsing is a pure function over the document ([`parse`]), so callers test
//! against captured output with no `sfdisk`; what each tool accepts or refuses stays with the tool.

use std::fmt;

use serde_json::Value;

/// One partition, with its counts in the dump's `unit` (sectors unless the dump says otherwise).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    /// `node`: the partition's device node.
    pub node: String,
    /// `uuid`: the partition GUID, lowercase; empty when the table has none.
    pub uuid: String,
    /// `type`: the partition type GUID, lowercase; empty when the table has none.
    pub kind: String,
    /// `start`: first sector, when reported.
    pub start: Option<u64>,
    /// `size`: sectors, when reported.
    pub size: Option<u64>,
}

/// A partition table as `sfdisk --json` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// `label`: `gpt`, `dos`; lowercase.
    pub label: String,
    /// `id`: the disk's GPT GUID, lowercase; empty when the table has none.
    pub id: String,
    /// `device`: the whole disk the table belongs to.
    pub device: String,
    /// `unit`: what `start` and `size` count.
    pub unit: Option<String>,
    /// `sectorsize`: bytes per sector, when the dump reports it.
    pub sector_bytes: Option<u64>,
    /// `firstlba`, `lastlba`: the first and last sectors a partition may use.
    pub first_usable_lba: Option<u64>,
    pub last_usable_lba: Option<u64>,
    pub partitions: Vec<Partition>,
}

/// Why a dump could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not JSON at all; the text is the parser's message.
    Json(String),
    NoTable,
    Partitions,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(text) => write!(f, "sfdisk JSON: {text}"),
            Self::NoTable => f.write_str("sfdisk JSON has no partition table"),
            Self::Partitions => f.write_str("sfdisk partitions are malformed"),
        }
    }
}

impl std::error::Error for Error {}

/// A whole count, whether the dump wrote it as a number or as decimal text.
fn as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

/// Reads a `sfdisk --json <disk>` document.
pub fn parse(json: &str) -> Result<Table, Error> {
    let root: Value = serde_json::from_str(json).map_err(|error| Error::Json(error.to_string()))?;
    let table = root
        .get("partitiontable")
        .filter(|table| table.is_object())
        .ok_or(Error::NoTable)?;
    let partitions = match table.get("partitions") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(items)) => items.as_slice(),
        Some(_) => return Err(Error::Partitions),
    };
    let mut parsed = Vec::with_capacity(partitions.len());
    for part in partitions {
        let part = part.as_object().ok_or(Error::Partitions)?;
        parsed.push(Partition {
            node: text(part.get("node")).unwrap_or_default(),
            uuid: text(part.get("uuid"))
                .unwrap_or_default()
                .to_ascii_lowercase(),
            kind: text(part.get("type"))
                .unwrap_or_default()
                .to_ascii_lowercase(),
            start: part.get("start").and_then(as_u64),
            size: part.get("size").and_then(as_u64),
        });
    }
    Ok(Table {
        label: text(table.get("label"))
            .unwrap_or_default()
            .to_ascii_lowercase(),
        id: text(table.get("id"))
            .unwrap_or_default()
            .to_ascii_lowercase(),
        device: text(table.get("device")).unwrap_or_default(),
        unit: text(table.get("unit")),
        sector_bytes: table.get("sectorsize").and_then(as_u64),
        first_usable_lba: table.get("firstlba").and_then(as_u64),
        last_usable_lba: table.get("lastlba").and_then(as_u64),
        partitions: parsed,
    })
}

/// The number at the end of a partition's device node: `/dev/nvme0n1p12` is 12.
pub fn node_number(node: &str) -> Option<u32> {
    let digits: String = node
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(json: &str) -> Table {
        parse(json).expect("fixture is an sfdisk document")
    }

    tilo_test_env::scenario! {
        fn sfdisk_tables_parse_into_labels_guids_and_partition_identities;

        phase reads_captured_tables_with_and_without_extents;
        {
            {
                let table = fixture(include_str!("../tests/fixtures/sfdisk.json"));
                assert_eq!(table.label, "gpt");
                assert_eq!(table.id, "a1b2c3d4-0000-4000-8000-00000000aaaa");
                assert_eq!(table.device, "/dev/nvme0n1");
                assert_eq!(table.unit.as_deref(), Some("sectors"));
                assert_eq!(table.sector_bytes, Some(512));
                assert_eq!((table.first_usable_lba, table.last_usable_lba), (Some(34), Some(1_000_215_182)));
                assert_eq!(table.partitions.len(), 2);
                let esp = &table.partitions[0];
                assert_eq!(esp.node, "/dev/nvme0n1p1");
                assert_eq!(esp.uuid, "0f0e0d0c-0000-4000-8000-000000000001");
                assert_eq!(esp.kind, "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
                assert_eq!((esp.start, esp.size), (Some(2048), Some(1_228_800)));
                assert_eq!(node_number(&esp.node), Some(1));
                assert_eq!(node_number("/dev/nvme0n1p12"), Some(12));
                assert_eq!(node_number("/dev/sda"), None);
            }

            {
                // The udevless installer's dump has identities and no extents.
                let table = fixture(
                    r#"{"partitiontable": {"label": "gpt", "id": "11111111-2222-4333-8444-555555555555",
                    "partitions": [{"node": "/dev/loop0p1", "uuid": "00000000-0000-4000-8000-000000000001",
                    "type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"}]}}"#,
                );
                assert_eq!(table.sector_bytes, None);
                assert_eq!(table.partitions[0].start, None);
                assert_eq!(table.partitions[0].kind, "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");

                // A count may arrive as decimal text, as `sfdisk` does for large tables.
                let table = fixture(
                    r#"{"partitiontable": {"label": "gpt", "partitions": [
                    {"node": "/dev/sda1", "start": "2048", "size": "4096"}]}}"#,
                );
                assert_eq!((table.partitions[0].start, table.partitions[0].size), (Some(2048), Some(4096)));
                assert_eq!(table.partitions[0].uuid, "");
            }
        }

        phase refuses_a_document_that_is_not_a_table;
        {
            assert!(matches!(parse("not json"), Err(Error::Json(_))));
            assert_eq!(parse(r#"{"partitions": []}"#).unwrap_err(), Error::NoTable);
            assert_eq!(parse(r#"{"partitiontable": []}"#).unwrap_err(), Error::NoTable);
            assert_eq!(
                parse(r#"{"partitiontable": {"partitions": 1}}"#).unwrap_err(),
                Error::Partitions
            );
            assert_eq!(
                parse(r#"{"partitiontable": {"partitions": [1]}}"#).unwrap_err(),
                Error::Partitions
            );
            assert_eq!(
                Error::NoTable.to_string(),
                "sfdisk JSON has no partition table"
            );
        }
    }
}
