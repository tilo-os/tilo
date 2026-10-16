//! One runtime registration and the original request's retained time identity.
use crate::peer_actions::PolicyTime;

pub(super) fn sample(
    registered: Option<PolicyTime>,
    submitted: Option<&PolicyTime>,
) -> Result<(Option<PolicyTime>, u64), String> {
    let retained = match (registered, submitted) {
        (Some(registered), Some(submitted)) if registered.same_origin(submitted) => {
            Some(submitted.clone())
        }
        (None, None) => None,
        _ => {
            return Err(
                "original policy clock or origin does not match runtime registration".into(),
            );
        }
    };
    let now = match &retained {
        Some(time) => time.now_ns().map_err(|error| error.to_string())?,
        None => tilo_clock::try_unix_now_ns().ok_or("legacy clock unavailable")?,
    };
    Ok((retained, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_actions::OwnerPolicyClock;
    #[test]
    fn retained_policy_context_requires_original_clock_and_origin() {
        let clock = OwnerPolicyClock::new().unwrap();
        let original = clock.new_origin();
        let first = original.capture().unwrap();
        assert!(sample(Some(first.clone()), Some(&first)).is_ok());
        assert!(
            sample(Some(first.clone()), None).is_err(),
            "required context cannot be recaptured at a consumer"
        );
        let replacement = clock.new_origin();
        assert!(
            sample(Some(replacement.capture().unwrap()), Some(&first)).is_err(),
            "same clock cannot rebind old work to new origin"
        );
        let other_clock = OwnerPolicyClock::new().unwrap();
        let other = other_clock.new_origin();
        assert!(sample(Some(other.capture().unwrap()), Some(&first)).is_err());
        assert!(
            sample(None, Some(&first)).is_err(),
            "legacy registration cannot consume required work"
        );
        original.invalidate();
        assert!(sample(Some(first.clone()), Some(&first)).is_err());
        assert!(sample(None, None).is_ok());
    }
}
