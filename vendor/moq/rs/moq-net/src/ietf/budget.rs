//! Per-session child-task admission; permits follow the future, including cancellation.
use crate::Error;
use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Default)]
pub(super) struct Budget {
	live: Arc<AtomicUsize>,
	maximum: Option<usize>,
}
pub(super) struct Permit(Arc<AtomicUsize>);
impl Budget {
	pub fn bounded(maximum: usize) -> Self {
		Self {
			maximum: Some(maximum),
			..Self::default()
		}
	}
	pub fn acquire(&self) -> Result<Permit, Error> {
		self.live
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
				if self.maximum.is_some_and(|maximum| live >= maximum) {
					None
				} else {
					live.checked_add(1)
				}
			})
			.map_err(|_| Error::ProtocolViolation)?;
		Ok(Permit(self.live.clone()))
	}
}
impl Drop for Permit {
	fn drop(&mut self) {
		self.0.fetch_sub(1, Ordering::AcqRel);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn task_budget_saturation_release_and_cancel() {
		let budget = Budget::bounded(32);
		let mut permits: Vec<_> = (0..32).map(|_| budget.acquire().unwrap()).collect();
		assert!(matches!(budget.acquire(), Err(Error::ProtocolViolation)));
		let clone = budget.clone();
		drop(permits.pop());
		let permit = clone.acquire().unwrap();
		// The child retains its permit after its submitting parent clone is dropped.
		let child = async move {
			let _permit = permit;
			std::future::pending::<()>().await;
		};
		drop(clone);
		assert!(budget.acquire().is_err());
		drop(child);
		assert!(budget.acquire().is_ok());
		drop(permits);
		assert_eq!(budget.live.load(Ordering::Acquire), 0);
	}
}
