//! Whether a slot's next generation can read the system-interface
//! declaration it ends up with.
//!
//! `/usr/share/tilo/system-interfaces.toml` is read by every component of
//! the matched session set ([`USERSPACE_COMPONENTS`]): the shell, the
//! launcher, `tilo-install`, the app host and the runtime services judge
//! apps by it. The file a slot sees is the one its layers ship (only the
//! shell does, `os/components.toml`), else the image's. A generation in
//! which one of those components was built for another declaration than
//! the one it reads is refused before anything is activated: on 2026-10-22
//! a shell layer from main shipped a one-version-per-interface declaration
//! over an image whose app host and `ush` read lists of versions, and the
//! next session start left the app host unable to read it.
//!
//! A component speaks the declaration of the tree it was built from
//! ([`Provenance::interfaces`]); the image's own components speak the
//! image's declaration, which the early-boot unit records before any layer
//! is merged ([`crate::image::recorded_declaration`]).
//!
//! The runtime services that run VM-wide ([`VM_RUNTIME_SERVICES`], tilo-fs)
//! serve apps of every slot, so they are judged too, by the runtime version
//! alone: a slot's swap is refused when the `vm` slot's fs (or the image's)
//! serves another runtime version than the slot's declaration, and an fs
//! swap is refused when it would leave any slot reading another one
//! ([`vm_conflict`]).

use std::collections::BTreeMap;

use tilo_vm_protocol::{Declaration, Provenance, SYSTEM_INTERFACES, USERSPACE_COMPONENTS};

/// One component of the generation being checked.
#[derive(Debug, Clone)]
pub struct Member {
    pub component: String,
    pub layer: String,
    /// What the layer records about its build; `None` when unreadable.
    pub provenance: Option<Provenance>,
    pub ships: Option<Declaration>,
}

/// The runtime services apps reach that run VM-wide, outside any slot's
/// session set: their components are `vm`-scope, swapped into the `vm` slot
/// and shared by every slot. Each speaks the runtime interface alone, so it
/// is judged by the [`RUNTIME`] version of the declaration a slot reads.
pub const VM_RUNTIME_SERVICES: &[&str] = &["fs"];

/// The interface every runtime service speaks to apps.
pub const RUNTIME: &str = "runtime";

/// The declaration a slot of `members` reads and whose it is: the one layer
/// that ships it, else the image's. `Err` when two layers ship it.
fn effective<'a>(
    image: Option<&'a Declaration>,
    members: &'a [Member],
) -> Result<(Option<&'a Declaration>, String, bool), String> {
    let shippers: Vec<&Member> = members.iter().filter(|m| m.ships.is_some()).collect();
    if let [first, second, ..] = shippers.as_slice() {
        return Err(format!(
            "both the {} layer and the {} layer ship {SYSTEM_INTERFACES}; one component owns it",
            first.component, second.component
        ));
    }
    Ok(match shippers.first() {
        Some(owner) => (
            owner.ships.as_ref(),
            format!("the {} layer's", owner.component),
            true,
        ),
        None => (image, "the image's".to_owned(), false),
    })
}

/// What `member` records about the interfaces it was built for; `None` when
/// it records none.
fn speaks(member: &Member) -> Option<&BTreeMap<String, u32>> {
    member
        .provenance
        .as_ref()
        .map(|p| &p.interfaces)
        .filter(|interfaces| !interfaces.is_empty())
}

/// Why the VM-wide runtime service `member` cannot serve a slot reading
/// `served`, if it cannot.
fn service_problem(member: &Member, served: Option<&BTreeMap<String, u32>>) -> Option<String> {
    let Some(speaks) = speaks(member) else {
        return Some(format!(
            "the {} layer {} records no interface versions",
            member.component,
            short(&member.layer)
        ));
    };
    let wanted = served.and_then(|served| served.get(RUNTIME));
    match speaks.get(RUNTIME) {
        Some(version) if Some(version) == wanted => None,
        Some(version) => Some(format!(
            "the {} layer {} serves {RUNTIME} {version}",
            member.component,
            short(&member.layer)
        )),
        None => Some(format!(
            "the {} layer {} was built for no {RUNTIME} interface",
            member.component,
            short(&member.layer)
        )),
    }
}

/// `None` when every component of `members` (a slot's generation), every
/// VM-wide runtime service in `vm_services` (the `vm` slot's) and every one
/// the image still provides reads or serves the declaration the slot ends up
/// with; otherwise what is wrong and what to swap together. `image` is the
/// image's own declaration, `None` when no boot has recorded it.
#[must_use]
pub fn conflict(
    image: Option<&Declaration>,
    members: &[Member],
    vm_services: &[Member],
) -> Option<String> {
    let (effective, from, shipped) = match effective(image, members) {
        Ok(found) => found,
        Err(two) => return Some(two),
    };
    let effective = match effective {
        // Nothing declares anything: nothing is judged against a declaration.
        None if !shipped => return None,
        Some(Declaration::Absent) => return None,
        other => other,
    };
    let served = effective.and_then(Declaration::versions);
    let mut problems = Vec::new();
    let in_set = |component: &str| USERSPACE_COMPONENTS.contains(&component);
    for member in members.iter().filter(|m| in_set(&m.component)) {
        match (speaks(member), served) {
            (None, _) => problems.push(format!(
                "the {} layer {} records no interface versions",
                member.component,
                short(&member.layer)
            )),
            (Some(speaks), Some(served)) if speaks == served => {}
            (Some(speaks), _) => problems.push(format!(
                "the {} layer {} was built for {}",
                member.component,
                short(&member.layer),
                describe(speaks)
            )),
        }
    }
    let services = vm_services
        .iter()
        .filter(|m| VM_RUNTIME_SERVICES.contains(&m.component.as_str()));
    problems.extend(services.filter_map(|member| service_problem(member, served)));
    if shipped {
        // The image's own components read the layer's declaration too.
        let image_speaks = image.map(|declaration| declaration.versions());
        let fits = match image_speaks {
            Some(Some(versions)) => served == Some(versions),
            // An image that declares nothing judges nothing.
            Some(None) if image == Some(&Declaration::Absent) => true,
            Some(None) | None => false,
        };
        if !fits {
            let from_image: Vec<&str> = USERSPACE_COMPONENTS
                .iter()
                .copied()
                .filter(|component| !members.iter().any(|m| m.component == *component))
                .chain(
                    VM_RUNTIME_SERVICES
                        .iter()
                        .copied()
                        .filter(|component| !vm_services.iter().any(|m| m.component == *component)),
                )
                .collect();
            if !from_image.is_empty() {
                let built_for = match image {
                    None => "a declaration no boot has recorded".to_owned(),
                    Some(Declaration::Unreadable { .. }) => {
                        "a declaration in another format".to_owned()
                    }
                    Some(declaration) => declaration
                        .versions()
                        .map_or_else(|| "no declaration".to_owned(), describe),
                };
                problems.push(format!(
                    "the image's own {} were built for {built_for}",
                    from_image.join(", ")
                ));
            }
        }
    }
    if problems.is_empty() {
        return None;
    }
    Some(format!(
        "the slot would read {from} {SYSTEM_INTERFACES}, which serves {}, but {}. {ADVICE}",
        reads(effective),
        problems.join("; ")
    ))
}

/// `None` when the VM-wide runtime services `vm_services` (the `vm` slot's
/// next generation) serve every slot of `slots` (each slot's name and
/// current generation) the runtime version the declaration it reads
/// serves; otherwise which slots they would leave behind.
#[must_use]
pub fn vm_conflict(
    image: Option<&Declaration>,
    slots: &[(String, Vec<Member>)],
    vm_services: &[Member],
) -> Option<String> {
    let services: Vec<&Member> = vm_services
        .iter()
        .filter(|m| VM_RUNTIME_SERVICES.contains(&m.component.as_str()))
        .collect();
    if services.is_empty() {
        return None;
    }
    let mut problems = Vec::new();
    for (slot, members) in slots {
        let (effective, from) = match effective(image, members) {
            Ok((effective, from, _)) => (effective, from),
            Err(two) => {
                problems.push(format!("slot '{slot}': {two}"));
                continue;
            }
        };
        let served = match effective {
            None | Some(Declaration::Absent) => continue,
            Some(declaration) => declaration.versions(),
        };
        for member in &services {
            if let Some(problem) = service_problem(member, served) {
                problems.push(format!(
                    "slot '{slot}' reads {from} declaration, which serves {}, and {problem}",
                    reads(effective)
                ));
            }
        }
    }
    (!problems.is_empty()).then(|| format!("{}. {ADVICE}", problems.join("; ")))
}

const ADVICE: &str = "Every component of the matched session set reads that file, and the \
                      runtime services apps reach (fs) serve its runtime version: swap them \
                      together (`tilo-vm swap userspace` builds the whole set from one tree, \
                      `tilo-vm swap fs` the filesystem service from the same tree), or boot an \
                      image built from the same tree as the layer";

fn reads(effective: Option<&Declaration>) -> String {
    match effective {
        Some(Declaration::Versions { versions }) => describe(versions),
        Some(Declaration::Unreadable { detail }) => {
            format!("a declaration this system cannot read ({detail})")
        }
        Some(Declaration::Absent) | None => "no declaration".to_owned(),
    }
}

/// `shell-ui 11, runtime 2, system 1`.
fn describe(versions: &BTreeMap<String, u32>) -> String {
    if versions.is_empty() {
        return "no interface".to_owned();
    }
    versions
        .iter()
        .map(|(name, version)| format!("{name} {version}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn short(layer: &str) -> &str {
    layer.get(..12).unwrap_or(layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    fn current() -> BTreeMap<String, u32> {
        versions(&[("runtime", 2), ("shell-ui", 11), ("system", 1)])
    }

    fn declared(pairs: &BTreeMap<String, u32>) -> Declaration {
        Declaration::Versions {
            versions: pairs.clone(),
        }
    }

    fn member(component: &str, speaks: &BTreeMap<String, u32>, ships: bool) -> Member {
        let mut provenance = Provenance::local(component);
        provenance.interfaces = speaks.clone();
        Member {
            component: component.to_owned(),
            layer: format!("{:0>64}", component.len()),
            provenance: Some(provenance),
            ships: ships.then(|| declared(speaks)),
        }
    }

    /// The image the user's laptop ran on 2026-10-22: built before
    /// interfaces had one version each, its declaration lists versions.
    fn list_format_image() -> Declaration {
        Declaration::parse("[interfaces]\nsystem = [1]\napp-host = [1]\nshell-ui = [1]\n")
    }

    #[test]
    fn a_shell_swap_over_an_image_whose_app_host_reads_another_declaration_is_refused() {
        let image = list_format_image();
        assert!(matches!(image, Declaration::Unreadable { .. }));
        let stack = [
            member("shell", &current(), true),
            member("launcher", &current(), false),
            member("install", &current(), false),
        ];
        let refused = conflict(Some(&image), &stack, &[]).expect("the swap is refused");
        assert!(refused.contains("the shell layer's"), "{refused}");
        assert!(refused.contains("shell-ui 11"), "{refused}");
        for from_image in ["app-host", "ush"] {
            assert!(
                refused.contains(from_image),
                "names {from_image} to swap together: {refused}"
            );
        }
        assert!(refused.contains("tilo-vm swap userspace"), "{refused}");
    }

    #[test]
    fn the_whole_session_set_swapped_together_reads_its_own_declaration() {
        let image = list_format_image();
        let stack: Vec<Member> = USERSPACE_COMPONENTS
            .iter()
            .map(|component| member(component, &current(), *component == "shell"))
            .collect();
        let fs = [member("fs", &current(), false)];
        assert_eq!(conflict(Some(&image), &stack, &fs), None);
    }

    #[test]
    fn a_layer_built_for_the_image_s_declaration_swaps_and_one_built_for_another_does_not() {
        let image = declared(&current());
        assert_eq!(
            conflict(Some(&image), &[member("app-host", &current(), false)], &[]),
            None
        );
        let older = versions(&[("runtime", 2), ("shell-ui", 10), ("system", 1)]);
        let refused = conflict(Some(&image), &[member("app-host", &older, false)], &[])
            .expect("an app host built for shell-ui 10 is refused on a shell-ui 11 image");
        assert!(refused.contains("the app-host layer"), "{refused}");
        assert!(refused.contains("shell-ui 10"), "{refused}");
    }

    #[test]
    fn a_layer_that_records_no_interface_versions_cannot_be_vouched_for() {
        let image = declared(&current());
        let refused = conflict(Some(&image), &[member("ush", &BTreeMap::new(), false)], &[])
            .expect("unknown is refused");
        assert!(
            refused.contains("records no interface versions"),
            "{refused}"
        );
    }

    #[test]
    fn an_unrecorded_image_under_a_layer_s_declaration_cannot_be_vouched_for() {
        let refused = conflict(None, &[member("shell", &current(), true)], &[])
            .expect("the image's own readers are unknown");
        assert!(refused.contains("no boot has recorded"), "{refused}");
    }

    #[test]
    fn two_layers_shipping_the_declaration_are_refused() {
        let image = declared(&current());
        let refused = conflict(
            Some(&image),
            &[
                member("shell", &current(), true),
                member("app-host", &current(), true),
            ],
            &[],
        )
        .expect("one owner");
        assert!(refused.contains("one component owns it"), "{refused}");
    }

    #[test]
    fn components_outside_the_session_set_and_undeclared_systems_are_not_judged() {
        let older = versions(&[("shell-ui", 1)]);
        assert_eq!(
            conflict(
                Some(&declared(&current())),
                &[member("vmd", &older, false)],
                &[]
            ),
            None
        );
        assert_eq!(
            conflict(
                Some(&Declaration::Absent),
                &[member("app-host", &older, false)],
                &[],
            ),
            None
        );
        assert_eq!(
            conflict(None, &[member("app-host", &older, false)], &[]),
            None
        );
    }

    /// tilo-fs is a runtime service apps talk to, though it runs VM-wide.
    /// A slot whose declaration serves another runtime version than the fs
    /// layer is refused, and so is an fs swap that would leave a slot behind.
    #[test]
    fn the_filesystem_service_serves_the_runtime_version_every_slot_reads() {
        let image = declared(&current());
        let next_runtime = versions(&[("runtime", 3), ("shell-ui", 11), ("system", 1)]);
        // Same runtime, other shell-ui: fs speaks only the runtime interface.
        let other_shell_ui = versions(&[("runtime", 2), ("shell-ui", 12), ("system", 1)]);
        assert_eq!(
            conflict(Some(&image), &[], &[member("fs", &other_shell_ui, false)]),
            None
        );
        let refused = conflict(Some(&image), &[], &[member("fs", &next_runtime, false)])
            .expect("an fs serving runtime 3 to a slot reading runtime 2 is refused");
        assert!(refused.contains("the fs layer"), "{refused}");
        assert!(refused.contains("runtime 3"), "{refused}");
        assert!(refused.contains("tilo-vm swap fs"), "{refused}");

        // A slot's whole session set moved to runtime 3 while fs stays the
        // image's: fs is named among what to swap together.
        let stack: Vec<Member> = USERSPACE_COMPONENTS
            .iter()
            .map(|component| member(component, &next_runtime, *component == "shell"))
            .collect();
        let refused = conflict(Some(&image), &stack, &[]).expect("the image's fs serves runtime 2");
        assert!(refused.contains("the image's own fs"), "{refused}");
        assert_eq!(
            conflict(Some(&image), &stack, &[member("fs", &next_runtime, false)]),
            None,
            "with fs from the same tree the slot is whole"
        );

        let slots = vec![
            ("device".to_owned(), Vec::new()),
            ("review-a".to_owned(), stack),
        ];
        let refused = vm_conflict(Some(&image), &slots, &[member("fs", &next_runtime, false)])
            .expect("the device slot still reads runtime 2");
        assert!(refused.contains("slot 'device'"), "{refused}");
        assert!(!refused.contains("slot 'review-a'"), "{refused}");
        assert_eq!(
            vm_conflict(
                Some(&image),
                &slots[..1],
                &[member("fs", &current(), false)]
            ),
            None
        );
    }
}
