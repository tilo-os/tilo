//! The person's devices read as one list: their copies.

use super::*;
use crate::fabric::{ClipboardPayload, FabricStamp};

fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}

fn clipboard(text: &str, origin: &DeviceId, created_unix_ms: u64) -> ClipboardEntry {
    ClipboardEntry::shareable(
        ClipboardPayload::Text(text.into()),
        origin.clone(),
        created_unix_ms,
        FabricStamp {
            counter: 1,
            device: origin.clone(),
        },
    )
    .unwrap()
}

#[test]
fn the_persons_devices_read_their_copies() {
    let laptop = id("laptop");
    let tablet = id("tablet");
    let mut laptop_device = PersonDevice::new(
        laptop.clone(),
        DeviceAbout {
            label: "Laptop".into(),
            kind: DeviceKind::Laptop,
            ..DeviceAbout::named("Laptop")
        },
    );
    laptop_device.online = true;
    laptop_device.clipboard = vec![clipboard("old", &laptop, 10)];

    let mut tablet_device = PersonDevice::new(tablet.clone(), DeviceAbout::named("Tablet"));
    tablet_device.clipboard = vec![clipboard("new", &tablet, 20)];

    let devices = PersonDevices::new(vec![laptop_device, tablet_device]);
    let merged: Vec<_> = devices
        .merged_clipboard()
        .entries()
        .map(|entry| entry.origin.as_str().to_owned())
        .collect();
    assert_eq!(merged, vec!["tablet", "laptop"], "newest copy first");
}
