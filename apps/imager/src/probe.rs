//! Native probing uses the canonical model without conversion.
use crate::model::{Source, TargetDrive};
use serde_json::Value;
use std::path::Path;

pub fn probe() -> Option<Vec<TargetDrive>> {
    imager_backend::probe::probe()
}
pub fn parse_diskutil(json: &Value) -> Option<Vec<TargetDrive>> {
    imager_backend::probe::parse_diskutil(json)
}
pub fn parse_windows_disks(json: &Value) -> Option<Vec<TargetDrive>> {
    imager_backend::probe::parse_windows_disks(json)
}
pub fn parse_lsblk(json: &Value) -> Option<Vec<TargetDrive>> {
    imager_backend::probe::parse_lsblk(json)
}
pub fn inspect_source(path: &Path) -> Option<Source> {
    imager_backend::probe::inspect_source(path)
}
pub use imager_backend::probe::read_iso_label;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_adapter_preserves_every_serialized_field() {
        let inventory = serde_json::json!({"blockdevices": [{
            "name": "sdb", "model": "Reviewed USB", "serial": "serial-1",
            "wwn": "wwn-1", "size": 64000000000_u64, "type": "disk",
            "rota": false, "tran": "usb", "rm": true,
            "children": [{
                "name": "sdb1", "size": 32000000000_u64, "type": "part",
                "fstype": "ext4", "label": "Documents", "mountpoint": null
            }]
        }]});
        let app = parse_lsblk(&inventory).expect("app inventory");
        let native = imager_backend::probe::parse_lsblk(&inventory).expect("native inventory");
        assert_eq!(app.len(), 1);
        assert_eq!(
            serde_json::to_value(&app).unwrap(),
            serde_json::to_value(&native).unwrap()
        );
        assert_eq!(
            serde_json::to_value(app[0].clone()).unwrap(),
            serde_json::to_value(&native[0]).unwrap()
        );
    }

    #[test]
    fn source_adapter_preserves_reviewed_identity() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("reviewed.iso");
        std::fs::write(&image, b"image bytes").unwrap();
        let app = inspect_source(&image).expect("app source");
        let native = imager_backend::probe::inspect_source(&image).expect("native source");
        assert_eq!(
            serde_json::to_value(app).unwrap(),
            serde_json::to_value(native).unwrap()
        );
    }
}
