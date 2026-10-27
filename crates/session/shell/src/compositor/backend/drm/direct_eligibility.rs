//! What makes one output eligible for direct scanout.

use super::DirectEligibility;

fn eligible() -> DirectEligibility<'static> {
    DirectEligibility {
        hardware_cursor: true,
        event_pageflips: true,
        renderer_pci: "0000:66:00.0",
        renderer_drm_matches: false,
        drm_bus_id: Some("pci:0000:66:00.0"),
    }
}

#[test]
fn direct_eligibility_is_per_scanout() {
    // Two outputs side by side: the plain one renders direct while its
    // sibling without a cursor plane names its own reason. Neither is judged
    // by the count of outputs it shares the desktop with.
    let plain = eligible();
    let software_cursor = DirectEligibility {
        hardware_cursor: false,
        ..eligible()
    };
    assert_eq!(direct_scanout_rejection(plain), None);
    assert_eq!(
        direct_scanout_rejection(software_cursor),
        Some("hardware cursor plane is unavailable")
    );
}

#[test]
fn direct_path_eligibility_requires_the_full_contract_and_proven_identity() {
    assert_eq!(direct_scanout_rejection(eligible()), None);
    for (rejected, reason) in [
        (
            DirectEligibility {
                hardware_cursor: false,
                ..eligible()
            },
            "hardware cursor plane is unavailable",
        ),
        (
            DirectEligibility {
                event_pageflips: false,
                ..eligible()
            },
            "event-driven page flips are unavailable",
        ),
        (
            DirectEligibility {
                drm_bus_id: Some("pci:0000:01:00.0"),
                ..eligible()
            },
            "renderer and scanout use different GPUs",
        ),
    ] {
        assert_eq!(direct_scanout_rejection(rejected), Some(reason));
    }
    assert_eq!(
        direct_scanout_rejection(DirectEligibility {
            renderer_pci: "",
            renderer_drm_matches: true,
            drm_bus_id: None,
            ..eligible()
        }),
        None
    );
    assert_eq!(
        direct_scanout_rejection(DirectEligibility {
            renderer_pci: "0000:66:00.0",
            renderer_drm_matches: true,
            drm_bus_id: None,
            ..eligible()
        }),
        None
    );
    assert_eq!(
        direct_scanout_rejection(DirectEligibility {
            renderer_pci: "",
            renderer_drm_matches: false,
            drm_bus_id: None,
            ..eligible()
        }),
        Some("renderer and scanout use different GPUs")
    );
}
