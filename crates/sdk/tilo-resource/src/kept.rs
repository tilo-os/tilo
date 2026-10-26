//! Action types kept per thread and generation of app code: the one
//! [`ActionType`] every item offering an action shares, built the first time
//! an item offers it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::thread::LocalKey;

use tilo_actions::ActionType;

/// The kept types of one kind of key, for one generation of app code
/// ([`reset_type_schemas`](crate::reset_type_schemas) starts the next).
pub(crate) struct KeptTypes<K, V = Arc<ActionType>> {
    epoch: u64,
    types: HashMap<K, V>,
}

impl<K, V> Default for KeptTypes<K, V> {
    fn default() -> Self {
        Self {
            epoch: 0,
            types: HashMap::new(),
        }
    }
}

/// The type `key` names in `kept`, built by `build` when this thread has
/// none of this generation. Built outside the borrow: building one may keep
/// another.
pub(crate) fn kept_type<K: Hash + Eq, V: Clone>(
    kept: &'static LocalKey<RefCell<KeptTypes<K, V>>>,
    key: K,
    build: impl FnOnce() -> V,
) -> V {
    let epoch = crate::resource::type_epoch();
    let found = kept.with(|kept| {
        let kept = kept.borrow();
        if kept.epoch == epoch {
            kept.types.get(&key).cloned()
        } else {
            None
        }
    });
    found.unwrap_or_else(|| {
        let ty = build();
        kept.with(|kept| {
            let mut kept = kept.borrow_mut();
            if kept.epoch != epoch {
                *kept = KeptTypes {
                    epoch,
                    types: HashMap::new(),
                };
            }
            kept.types.insert(key, ty.clone());
        });
        ty
    })
}

/// Types kept per vocabulary and dotted path or field: `None` where the
/// path offers no such action, so it is not looked for again.
pub(crate) type KeptByPath = KeptTypes<(std::any::TypeId, String), Option<Arc<ActionType>>>;
