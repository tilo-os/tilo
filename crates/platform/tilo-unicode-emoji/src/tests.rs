//! What offering leaves the shared picker with, per target.

use super::*;

/// Offering hands the toolkit's picker the whole Unicode catalogue.
#[test]
fn offering_hands_over_the_catalogue() {
    offer();
    let catalogue = ui::emoji_catalogue::catalogue();
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
}
