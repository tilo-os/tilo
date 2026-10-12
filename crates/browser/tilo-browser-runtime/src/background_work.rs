//! Ordinary owner-model service is scheduled separately from canvas presentation.
#[derive(Clone, Copy)]
pub(crate) struct Presentation {
    pub hidden: bool,
    pub suspended: bool,
    pub gpu_ready: bool,
}
impl Presentation {
    pub(crate) const fn available(self) -> bool {
        !self.hidden && !self.suspended && self.gpu_ready
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub serviced: usize,
    pub paint_available: bool,
}

pub(crate) fn run<E>(
    presentation: Presentation,
    service: impl FnOnce() -> Result<usize, E>,
) -> Result<Outcome, E> {
    let serviced = service()?;
    Ok(Outcome {
        serviced,
        paint_available: presentation.available(),
    })
}
