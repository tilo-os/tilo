//! Creation-time preflight for the adapters that forward host TCP to guest SSH.
//! This checks availability, not an atomic reservation until the VM starts.

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

use crate::config::Registry;
use crate::error::CliError;

const DEFAULT_PORT: u16 = 2222;

fn forwards_ssh(adapter: &str) -> bool {
    matches!(adapter, "libvirt" | "qemu" | "whpx")
}

pub(crate) fn select(
    registry: &Registry,
    adapter: &str,
    requested: Option<u16>,
) -> Result<Option<u16>, CliError> {
    choose(registry, adapter, requested, available)
}

fn choose(
    registry: &Registry,
    adapter: &str,
    requested: Option<u16>,
    mut probe: impl FnMut(&str, u16) -> io::Result<bool>,
) -> Result<Option<u16>, CliError> {
    if !forwards_ssh(adapter) {
        return Ok(requested);
    }
    if requested == Some(0) {
        return Err(CliError::InvalidInput(
            "--ssh-port must be a nonzero host TCP port".into(),
        ));
    }
    let registered = |port| {
        registry.vms.values().any(|entry| {
            forwards_ssh(&entry.hypervisor) && entry.ssh_port.unwrap_or(DEFAULT_PORT) == port
        })
    };
    for port in requested.unwrap_or(DEFAULT_PORT)..=requested.unwrap_or(u16::MAX) {
        if registered(port) {
            if requested.is_some() {
                return Err(CliError::InvalidInput(format!(
                    "--ssh-port {port} is already used by a registered VM; choose another port"
                )));
            }
            continue;
        }
        if probe(adapter, port).map_err(CliError::io(format!(
            "check host TCP port {port} for {adapter} SSH forwarding"
        )))? {
            return Ok(Some(port));
        }
        if requested.is_some() {
            return Err(CliError::InvalidInput(format!(
                "--ssh-port {port} is busy on this host; choose another port or omit --ssh-port"
            )));
        }
    }
    Err(CliError::InvalidInput(
        "no available host SSH forwarding port from 2222 through 65535".into(),
    ))
}

fn available(adapter: &str, port: u16) -> io::Result<bool> {
    // passt has no configured listen address; QEMU and WHPX bind IPv4 loopback.
    let address = if adapter == "libvirt" {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    match TcpListener::bind((address, port)) {
        Ok(listener) => drop(listener),
        Err(error) if error.kind() == ErrorKind::AddrInUse => return Ok(false),
        Err(error) => return Err(error),
    }
    if adapter == "libvirt" {
        // Release the IPv4 probe before a dual-stack IPv6 bind. Hosts without
        // IPv6 can still use the existing IPv4 forwarding configuration.
        match TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)) {
            Ok(listener) => drop(listener),
            Err(error) if error.kind() == ErrorKind::AddrInUse => return Ok(false),
            Err(error) if ipv6_unavailable(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn ipv6_unavailable(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
    ) {
        return true;
    }
    // EAFNOSUPPORT is not mapped to ErrorKind::Unsupported on Linux. Only
    // libvirt uses this probe, and its adapter is Linux-only.
    cfg!(target_os = "linux") && error.raw_os_error() == Some(97)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VmEntry;

    #[test]
    fn selection_skips_registered_and_busy_ports_for_each_forwarding_adapter() {
        let mut registry = Registry::default();
        registry.vms.insert(
            "old-default".into(),
            VmEntry {
                hypervisor: "libvirt".into(),
                ..VmEntry::default()
            },
        );
        registry.vms.insert(
            "stopped".into(),
            VmEntry {
                hypervisor: "whpx".into(),
                ssh_port: Some(2224),
                ..VmEntry::default()
            },
        );
        for adapter in ["libvirt", "qemu", "whpx"] {
            let mut probed = Vec::new();
            let port = choose(&registry, adapter, None, |actual, port| {
                assert_eq!(actual, adapter);
                probed.push(port);
                Ok(port != 2223)
            })
            .unwrap();
            assert_eq!(port, Some(2225));
            assert_eq!(probed, [2223, 2225]);
            assert!(
                choose(&registry, adapter, Some(2224), |_, _| panic!(
                    "registered port probed"
                ))
                .is_err()
            );
            assert!(choose(&registry, adapter, Some(2223), |_, _| Ok(false)).is_err());
            assert_eq!(
                choose(&registry, adapter, Some(22591), |_, _| Ok(true)).unwrap(),
                Some(22591)
            );
        }
    }

    #[test]
    fn other_adapters_keep_options_and_probe_errors_are_not_free_ports() {
        let registry = Registry::default();
        for adapter in ["utm", "hyperv", "device"] {
            for requested in [None, Some(2222)] {
                assert_eq!(
                    choose(&registry, adapter, requested, |_, _| panic!(
                        "not forwarded"
                    ))
                    .unwrap(),
                    requested
                );
            }
        }
        assert!(
            choose(&registry, "qemu", Some(0), |_, _| panic!(
                "port zero probed"
            ))
            .is_err()
        );
        let error = choose(&registry, "libvirt", None, |_, _| {
            Err(io::Error::from(ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("check host TCP port 2222"),
            "{error}"
        );
        assert!(ipv6_unavailable(&io::Error::from(
            ErrorKind::AddrNotAvailable
        )));
        assert!(!ipv6_unavailable(&io::Error::from(
            ErrorKind::PermissionDenied
        )));
        assert!(!ipv6_unavailable(&io::Error::from(ErrorKind::AddrInUse)));
        if cfg!(target_os = "linux") {
            assert!(ipv6_unavailable(&io::Error::from_raw_os_error(97)));
        }
    }

    #[test]
    fn an_actual_listener_refuses_the_exact_explicit_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        for adapter in ["libvirt", "qemu", "whpx"] {
            let error = select(&Registry::default(), adapter, Some(port)).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("--ssh-port {port} is busy")),
                "{error}"
            );
        }
    }
}
