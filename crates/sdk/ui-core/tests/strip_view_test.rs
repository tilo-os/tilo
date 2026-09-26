#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;
use tilo_ui::widget::{STRIP_COOLDOWN_MS, StripView};

#[test]
fn wheel_pagination_respects_cooldown_and_bounds() {
    // Case `wheel_paginate_cooldown_blocks_second_call`.
    {
        let mut v = StripView::new(vec!["a".into(), "b".into(), "c".into()], 0);
        assert!(v.try_paginate(1, Duration::from_millis(0)));
        assert_eq!(v.active_index(), 1);
        // second call inside cooldown → blocked
        assert!(!v.try_paginate(1, Duration::from_millis(100)));
        assert_eq!(v.active_index(), 1);
        // past cooldown → allowed
        assert!(v.try_paginate(1, Duration::from_millis(STRIP_COOLDOWN_MS + 10)));
        assert_eq!(v.active_index(), 2);
        // clamps at end
        assert!(!v.try_paginate(1, Duration::from_secs(10)));
        assert_eq!(v.active_index(), 2);
    }

    // Case `wheel_paginate_negative_wraps_none`.
    {
        let mut v = StripView::new(vec!["a".into(), "b".into()], 0);
        assert!(!v.try_paginate(-1, Duration::from_millis(0)));
        assert_eq!(v.active_index(), 0);
    }
}
