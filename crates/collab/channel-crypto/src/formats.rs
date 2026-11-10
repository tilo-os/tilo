//! How saved state written by an earlier build is brought to the current
//! format.
//!
//! State a device keeps (its channel peers' records) starts with one byte
//! naming the format it was written in. A build that changes that format
//! raises the version and adds one step to the format's upgrade path: a
//! function that reads state of the previous version and writes it as the
//! next, version byte included. Loading applies the steps in order from
//! whatever version the state names up to the current one, and the caller
//! saves the result in the current format, so a device keeps everything it
//! held across an upgrade. Nothing is dropped for being old: a host may be
//! the only always-online holder of a workspace's keys. State a step does not
//! reach (a version no build wrote, or one older than the path keeps) is
//! [`Error::OtherFormat`](crate::Error::OtherFormat).

use zeroize::Zeroizing;

use crate::{Error, Result};

/// One step of a saved format's upgrade path.
pub(crate) struct Step {
    /// The version the step reads; it writes `from + 1`.
    pub(crate) from: u8,
    /// State of version `from`, written as version `from + 1`.
    pub(crate) upgrade: fn(&[u8]) -> Result<Vec<u8>>,
}

/// `state`, whose first byte names its format, brought to `current` with
/// `path`, and the version it was written in when that was an earlier one.
///
/// # Errors
///
/// [`Error::OtherFormat`] for a version `path` does not bring to `current`,
/// or what a step returns.
pub(crate) fn upgrade(
    current: u8,
    path: &[Step],
    mut state: Zeroizing<Vec<u8>>,
) -> Result<(Zeroizing<Vec<u8>>, Option<u8>)> {
    let written = *state.first().ok_or(Error::OtherFormat)?;
    let mut version = written;
    while version != current {
        let step = path
            .iter()
            .find(|step| step.from == version)
            .ok_or(Error::OtherFormat)?;
        // State may hold key material: every form of it is wiped once dropped.
        state = Zeroizing::new((step.upgrade)(&state)?);
        let next = version.checked_add(1).ok_or(Error::OtherFormat)?;
        if state.first() != Some(&next) {
            return Err(Error::InvalidState(format!(
                "the upgrade from format {version} wrote no state of format {next}"
            )));
        }
        version = next;
    }
    Ok((state, (written != current).then_some(written)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step that raises the version and records it, as a real one writes
    /// the next format.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "a step's signature returns a Result"
    )]
    fn bump(state: &[u8]) -> Result<Vec<u8>> {
        let mut next = state.to_vec();
        let version = next.first().map_or(0, |version| version.saturating_add(1));
        if let Some(first) = next.first_mut() {
            *first = version;
        }
        next.push(version);
        Ok(next)
    }

    #[test]
    fn state_of_each_earlier_format_is_brought_to_the_current_one_in_order() {
        let path = [
            Step {
                from: 1,
                upgrade: bump,
            },
            Step {
                from: 2,
                upgrade: bump,
            },
        ];
        let up = |state: Vec<u8>| {
            upgrade(3, &path, Zeroizing::new(state)).map(|(state, from)| (state.to_vec(), from))
        };
        assert_eq!(up(vec![1]).unwrap(), (vec![3, 2, 3], Some(1)));
        assert_eq!(up(vec![2]).unwrap(), (vec![3, 3], Some(2)));
        assert_eq!(up(vec![3, 9]).unwrap(), (vec![3, 9], None));
        assert!(matches!(up(vec![4]), Err(Error::OtherFormat)));
        assert!(matches!(up(vec![0]), Err(Error::OtherFormat)));
        assert!(matches!(up(Vec::new()), Err(Error::OtherFormat)));
    }
}
