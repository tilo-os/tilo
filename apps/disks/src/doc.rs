//! Automerge document layer for the Disks app.
//!
//! The drives on this machine, their partitions and what is mounted are live
//! device state the app probes; none of it is in the document. What the
//! person authors is: the names they give their drives, keyed by the drive's
//! serial number so a USB stick keeps its name on every device it is plugged
//! into.
//!
//! ```text
//! root
//! └── "names"  (Map)   # drive serial -> name (Str)
//! ```

use std::collections::BTreeMap;

use ui::automerge::transaction::Transactable;
use ui::automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::doc::read_str;

const NAMES: &str = "names";

pub struct DisksDoc;

impl DisksDoc {
    /// Every named drive: serial number to name.
    pub fn names(doc: &AutoCommit) -> BTreeMap<String, String> {
        let Ok(Some((Value::Object(ObjType::Map), names))) = doc.get(&ROOT, NAMES) else {
            return BTreeMap::new();
        };
        doc.keys(&names)
            .filter_map(|serial| {
                let name = read_str(doc, &names, serial.as_str())?;
                Some((serial, name))
            })
            .collect()
    }

    /// Name the drive with `serial`, or forget its name (`None`).
    ///
    /// # Errors
    /// Returns the document's refusal of the write.
    pub fn set_name(
        doc: &mut AutoCommit,
        serial: &str,
        name: Option<&str>,
    ) -> Result<(), ui::automerge::AutomergeError> {
        let names = match doc.get(&ROOT, NAMES)? {
            Some((Value::Object(ObjType::Map), names)) => names,
            _ => doc.put_object(&ROOT, NAMES, ObjType::Map)?,
        };
        match name {
            Some(name) => doc.put(&names, serial, name),
            None => doc.delete(&names, serial),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_names_round_trip_and_clear() {
        let mut doc = AutoCommit::new();
        assert!(DisksDoc::names(&doc).is_empty());
        DisksDoc::set_name(&mut doc, "S4EWNX0R", Some("Work SSD")).unwrap();
        DisksDoc::set_name(&mut doc, "USB-1", Some("Backups")).unwrap();
        let names = DisksDoc::names(&doc);
        assert_eq!(names.get("S4EWNX0R").map(String::as_str), Some("Work SSD"));
        assert_eq!(names.len(), 2);
        DisksDoc::set_name(&mut doc, "USB-1", None).unwrap();
        assert_eq!(DisksDoc::names(&doc).len(), 1);
    }
}
