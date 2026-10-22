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

use std::collections::BTreeMap;

use tilo_vm_protocol::{Declaration, Provenance, SYSTEM_INTERFACES, USERSPACE_COMPONENTS};

/// One component of the generation being checked.
#[derive(Debug, Clone)]
pub struct Member {
    pub component: String,
    pub layer: String,
    /// What the layer records about its build; `None` when unreadable.
    pub provenance: Option<Provenance>,
    /// The declaration the layer ships, if it ships one.
    pub ships: Option<Declaration>,
}

/// `None` when every component of `members` and every one the image still
/// provides reads the declaration the generation ends up with; otherwise
/// what is wrong and what to swap together. `image` is the image's own
/// declaration, `None` when no boot has recorded it.
#[must_use]
pub fn conflict(image: Option<&Declaration>, members: &[Member]) -> Option<String> {
    let shippers: Vec<&Member> = members.iter().filter(|m| m.ships.is_some()).collect();
    if let [first, second, ..] = shippers.as_slice() {
        return Some(format!(
            "both the {} layer and the {} layer ship {SYSTEM_INTERFACES}; one component owns it",
            first.component, second.component
        ));
    }
    let (effective, from) = match shippers.first() {
        Some(owner) => (
            owner.ships.as_ref(),
            format!("the {} layer's", owner.component),
        ),
        None => (image, "the image's".to_owned()),
    };
    let effective = match effective {
        // Nothing declares anything: nothing is judged against a declaration.
        None if shippers.is_empty() => return None,
        Some(Declaration::Absent) => return None,
        other => other,
    };
    let served = effective.and_then(Declaration::versions);
    let mut problems = Vec::new();
    let in_set = |component: &str| USERSPACE_COMPONENTS.contains(&component);
    for member in members.iter().filter(|m| in_set(&m.component)) {
        let speaks = member
            .provenance
            .as_ref()
            .map(|p| &p.interfaces)
            .filter(|interfaces| !interfaces.is_empty());
        match (speaks, served) {
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
    if !shippers.is_empty() {
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
    let reads = match effective {
        Some(Declaration::Versions { versions }) => describe(versions),
        Some(Declaration::Unreadable { detail }) => {
            format!("a declaration this system cannot read ({detail})")
        }
        Some(Declaration::Absent) | None => "no declaration".to_owned(),
    };
    Some(format!(
        "the slot would read {from} {SYSTEM_INTERFACES}, which serves {reads}, but {}. \
         Every component of the matched session set reads that file: swap them together \
         (`tilo-vm swap userspace` builds the whole set from one tree), or boot an image built \
         from the same tree as the layer",
        problems.join("; ")
    ))
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

    // USER REQUIREMENT (2026-10-22): "the Tilo app repository stays available
    // after a component swap"
    #[test]
    fn a_shell_swap_over_an_image_whose_app_host_reads_another_declaration_is_refused() {
        let image = list_format_image();
        assert!(matches!(image, Declaration::Unreadable { .. }));
        let stack = [
            member("shell", &current(), true),
            member("launcher", &current(), false),
            member("install", &current(), false),
        ];
        let refused = conflict(Some(&image), &stack).expect("the swap is refused");
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

    // USER REQUIREMENT (2026-10-22): "the Tilo app repository stays available
    // after a component swap"
    #[test]
    fn the_whole_session_set_swapped_together_reads_its_own_declaration() {
        let image = list_format_image();
        let stack: Vec<Member> = USERSPACE_COMPONENTS
            .iter()
            .map(|component| member(component, &current(), *component == "shell"))
            .collect();
        assert_eq!(conflict(Some(&image), &stack), None);
    }

    #[test]
    fn a_layer_built_for_the_image_s_declaration_swaps_and_one_built_for_another_does_not() {
        let image = declared(&current());
        assert_eq!(
            conflict(Some(&image), &[member("app-host", &current(), false)]),
            None
        );
        let older = versions(&[("runtime", 2), ("shell-ui", 10), ("system", 1)]);
        let refused = conflict(Some(&image), &[member("app-host", &older, false)])
            .expect("an app host built for shell-ui 10 is refused on a shell-ui 11 image");
        assert!(refused.contains("the app-host layer"), "{refused}");
        assert!(refused.contains("shell-ui 10"), "{refused}");
    }

    #[test]
    fn a_layer_that_records_no_interface_versions_cannot_be_vouched_for() {
        let image = declared(&current());
        let refused = conflict(Some(&image), &[member("ush", &BTreeMap::new(), false)])
            .expect("unknown is refused");
        assert!(
            refused.contains("records no interface versions"),
            "{refused}"
        );
    }

    #[test]
    fn an_unrecorded_image_under_a_layer_s_declaration_cannot_be_vouched_for() {
        let refused = conflict(None, &[member("shell", &current(), true)])
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
        )
        .expect("one owner");
        assert!(refused.contains("one component owns it"), "{refused}");
    }

    #[test]
    fn components_outside_the_session_set_and_undeclared_systems_are_not_judged() {
        let older = versions(&[("shell-ui", 1)]);
        assert_eq!(
            conflict(Some(&declared(&current())), &[member("vmd", &older, false)]),
            None
        );
        assert_eq!(
            conflict(
                Some(&Declaration::Absent),
                &[member("app-host", &older, false)]
            ),
            None
        );
        assert_eq!(conflict(None, &[member("app-host", &older, false)]), None);
    }
}
