//! The `disks.system` resource: the names a person gives the storage devices
//! their devices see, as the app that draws them and an agent that renames them
//! both read it.
//!
//! A name follows the drive to every device it is plugged into. What a machine
//! has mounted, formatted or ejected stays in the app: those change one machine,
//! never the workspace's data.

use std::collections::BTreeMap;

use tilo_resource::{self, Atom, Model, Refusal, resource};

/// What the person authors about their storage devices.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Disks::valid)]
pub struct Disks {
    /// Drive serial number to the name the person gave it. A name is written whole: two
    /// people naming one drive at once keep one of the names.
    pub names: BTreeMap<String, Atom>,
}

impl Disks {
    /// A name is shown on one line: never blank, never padded.
    fn valid(&self) -> Result<(), Refusal> {
        if self
            .names
            .values()
            .any(|name| name.0.trim().is_empty() || name.0.trim() != name.0)
        {
            return Err(Refusal::new("a drive's name is not blank and not padded"));
        }
        Ok(())
    }

    #[must_use]
    pub fn names(&self) -> BTreeMap<String, String> {
        self.names
            .iter()
            .map(|(serial, name)| (serial.clone(), name.0.clone()))
            .collect()
    }

    /// What the resource holds: the names the person gives their drives.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(kind = "disks.system", read_content = Disks::read_document)]
impl Disks {
    /// Name a drive, or forget its name.
    ///
    /// The name follows the drive to every device it is plugged into.
    #[action]
    pub fn name_drive(
        &mut self,
        #[param("The drive's serial number")] serial: String,
        #[param("The name to show; none or blank forgets it")] name: Option<String>,
    ) {
        match name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            Some(name) => {
                self.names.insert(serial, Atom(name.to_owned()));
            }
            None => {
                self.names.remove(&serial);
            }
        }
    }
}
