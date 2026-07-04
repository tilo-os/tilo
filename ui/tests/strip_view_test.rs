use ui::widget::{StripView, STRIP_COOLDOWN_MS};
use std::time::Duration;

#[test]
fn wheel_paginate_cooldown_blocks_second_call() {
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
    assert!(!v.try_paginate(1, Duration::from_millis(10_000)));
    assert_eq!(v.active_index(), 2);
}

#[test]
fn wheel_paginate_negative_wraps_none() {
    let mut v = StripView::new(vec!["a".into(), "b".into()], 0);
    assert!(!v.try_paginate(-1, Duration::from_millis(0)));
    assert_eq!(v.active_index(), 0);
}
