//! A failure dropped on purpose, with the reason it may be.
//!
//! Session, collaboration and app-hosting code hears every failure it can
//! (`tools/policy/check_swallowed_failures.py`): a dropped error looks like an
//! absent value or a default, and whatever depended on it silently does
//! nothing. Some failures are fine to drop -- a probe of a file's shape, a
//! write another message supersedes, a destination that cannot fail -- and the
//! code says so with [`ignored!`], which keeps the expression and names the
//! reason with it. The reason is code the compiler keeps, so tidying the
//! comments around an expression never takes it away, and the check reads the
//! call rather than a comment.
//!
//! ```
//! use tilo_failure::ignored;
//!
//! let port = ignored!("a port that is not a number serves nothing here", "80".parse::<u16>().ok());
//! assert_eq!(port, Some(80));
//! ```

/// `body` with the reason its failure is dropped rather than handed on, logged,
/// answered or refused.
///
/// `reason` is a string literal a reader can check: the policy check leaves a
/// finding alone only for one that says why, and a bare word leaves it where
/// it was. Nothing else changes -- the call is the expression it wraps, to the
/// token, so it stands where that expression stood.
///
/// ```
/// use tilo_failure::ignored;
///
/// fn config(text: &str) -> Option<u32> {
///     Some(ignored!("an override that is not a number keeps the default", text.parse().ok())?)
/// }
///
/// assert_eq!(config("7"), Some(7));
/// assert_eq!(config("off"), None);
/// ```
#[macro_export]
macro_rules! ignored {
    ($reason:literal, $body:expr) => {
        $body
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_call_is_the_expression_it_wraps() {
        let parsed = crate::ignored!(
            "text that is not a number names nothing this test reads",
            "12".parse::<u32>()
        );
        assert_eq!(parsed, Ok(12));
        let missing = crate::ignored!(
            "text that is not a number names nothing this test reads",
            "no".parse::<u32>().ok()
        );
        assert_eq!(missing, None);
    }
}
