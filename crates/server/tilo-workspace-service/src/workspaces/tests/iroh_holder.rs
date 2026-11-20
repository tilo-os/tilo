//! A holder that serves no site, named by its Iroh address: its device key
//! and the endpoint it is dialed at, as `tilo-enroll device-key` prints
//! them, with the relay and the addresses it answers at when they are named.

use crate::workspaces::{IROH_HOLDER, IrohHolder};

/// A holder's endpoint id: a key of its own, not its device key.
fn endpoint(seed: u8) -> iroh_base::EndpointId {
    iroh_base::SecretKey::from_bytes(&[seed; 32]).public()
}

#[test]
fn a_holder_is_named_by_its_device_and_its_endpoint() {
    let device = [7; 32];
    let named = format!(
        "{IROH_HOLDER}{}@{}",
        hex::encode(device),
        hex::encode(endpoint(9).as_bytes())
    );
    let holder = IrohHolder::parse(&named)
        .expect("it is spelled as an Iroh holder")
        .unwrap();
    assert_eq!(holder.device, device);
    assert_eq!(holder.addr, iroh_base::EndpointAddr::new(endpoint(9)));
    assert_eq!(holder.to_string(), named, "it reads back as it was named");
}

#[test]
fn its_relay_and_direct_addresses_are_dialed_as_named() {
    let holder = IrohHolder {
        device: [3; 32],
        addr: iroh_base::EndpointAddr::new(endpoint(4))
            .with_relay_url("https://relay.example.test/".parse().unwrap())
            .with_ip_addr("192.0.2.10:4433".parse().unwrap()),
    };
    let named = holder.to_string();
    assert_eq!(
        named,
        format!(
            "{IROH_HOLDER}{}@{}?relay=https%3A%2F%2Frelay.example.test%2F&addr=192.0.2.10%3A4433",
            hex::encode([3; 32]),
            hex::encode(endpoint(4).as_bytes())
        )
    );
    assert_eq!(IrohHolder::parse(&named).unwrap().unwrap(), holder);
}

#[test]
fn a_site_is_no_iroh_holder_and_a_damaged_address_says_why() {
    assert!(IrohHolder::parse("https://tilo.sh").is_none());
    assert!(IrohHolder::parse("0123").is_none());
    let endpoint = hex::encode(endpoint(1).as_bytes());
    for damaged in [
        format!("{IROH_HOLDER}{endpoint}"),
        format!("{IROH_HOLDER}abcd@{endpoint}"),
        format!("{IROH_HOLDER}{}@nothing", hex::encode([1; 32])),
        format!(
            "{IROH_HOLDER}{}@{endpoint}?relay=not%20a%20url",
            hex::encode([1; 32])
        ),
        format!(
            "{IROH_HOLDER}{}@{endpoint}?addr=nowhere",
            hex::encode([1; 32])
        ),
        format!(
            "{IROH_HOLDER}{}@{endpoint}?web=https://tilo.sh",
            hex::encode([1; 32])
        ),
    ] {
        let refused = IrohHolder::parse(&damaged).expect("spelled as an Iroh holder");
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.contains("iroh:<device key>@<endpoint id>")),
            "{damaged}: {refused:?}"
        );
    }
}
