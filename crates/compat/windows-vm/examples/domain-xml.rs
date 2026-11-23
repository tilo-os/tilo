//! Render a domain for schema validation without contacting libvirt or creating a VM.
use windows_vm::{ApplianceConfig, GuestArchitecture, render_domain_xml};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let architecture = match arguments.next().as_deref() {
        Some("x86_64") => GuestArchitecture::X86_64,
        Some("aarch64") => GuestArchitecture::Aarch64,
        _ => return Err("usage: domain-xml <x86_64|aarch64>".into()),
    };
    if arguments.next().is_some() {
        return Err("usage: domain-xml <x86_64|aarch64>".into());
    }
    let configuration = ApplianceConfig::recommended_for(
        architecture,
        "01234567-89ab-cdef-0123-456789abcdef",
        "/var/lib/tilo/windows/system.qcow2",
        "/var/lib/tilo/windows/firmware-variables",
    );
    println!("{}", render_domain_xml(&configuration)?);
    Ok(())
}
