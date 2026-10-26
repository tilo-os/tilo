//! What offering leaves the shared picker with, per target.

use super::*;

/// A build that carries the catalogue hands the whole of it over; a build
/// that carries none leaves the picker without a catalogue rather than with
/// an empty one, which the picker tells apart.
#[test]
fn offering_hands_over_the_catalogue_this_build_carries() {
    offer();
    let offered = ui::emoji_catalogue::catalogue();
    if cfg!(not(target_arch = "wasm32")) {
        let catalogue = offered.expect("a native build carries the catalogue");
        assert!(
            !catalogue.groups().is_empty(),
            "every Unicode group is there"
        );
        assert!(
            catalogue.search("thumbs", 8).contains(&"👍"),
            "the offered catalogue finds emoji by name"
        );
        assert!(
            catalogue.info("👍").is_some_and(|info| info.takes_tone),
            "and knows their skin tones"
        );
    } else {
        assert!(
            offered.is_none(),
            "a build that carries no catalogue offers none, not an empty one"
        );
    }
}
