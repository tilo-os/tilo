//! Opaque home-adoption custody shared by setup frontends on every host.
use serde_json::Value;
pub struct PromotionReview {
    #[cfg(target_os = "linux")]
    inner: crate::home_transfer::promotion::PromotionReview,
    #[cfg(not(target_os = "linux"))]
    unavailable: std::convert::Infallible,
}
pub struct PromotionWorker {
    #[cfg(target_os = "linux")]
    inner: crate::home_transfer::promotion::PromotionWorker,
    #[cfg(not(target_os = "linux"))]
    unavailable: std::convert::Infallible,
}
pub struct PromotionReport {
    #[cfg(target_os = "linux")]
    inner: crate::home_transfer::promotion::PromotionReport,
    #[cfg(not(target_os = "linux"))]
    unavailable: std::convert::Infallible,
}
pub enum PromotionEvent {
    Finished(PromotionReport),
    Failed(String),
}
impl PromotionReview {
    #[cfg(target_os = "linux")]
    pub(crate) fn new(inner: crate::home_transfer::promotion::PromotionReview) -> Self {
        Self { inner }
    }
    pub fn preview(&self) -> Result<PromotionReport, String> {
        #[cfg(target_os = "linux")]
        {
            self.inner.preview().map(|inner| PromotionReport { inner })
        }
        #[cfg(not(target_os = "linux"))]
        {
            match self.unavailable {}
        }
    }
    pub fn start(&self, retry: bool) -> Result<PromotionWorker, String> {
        #[cfg(target_os = "linux")]
        {
            self.inner
                .start(retry)
                .map(|inner| PromotionWorker { inner })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = retry;
            match self.unavailable {}
        }
    }
}
impl PromotionWorker {
    pub fn cancel(&self) {
        #[cfg(target_os = "linux")]
        {
            self.inner.cancel();
        }
        #[cfg(not(target_os = "linux"))]
        match self.unavailable {}
    }
    pub fn try_next(&mut self) -> Option<PromotionEvent> {
        #[cfg(target_os = "linux")]
        {
            self.inner.try_next().map(|event| match event {
                crate::home_transfer::promotion::PromotionEvent::Finished(inner) => {
                    PromotionEvent::Finished(PromotionReport { inner })
                }
                crate::home_transfer::promotion::PromotionEvent::Failed(error) => {
                    PromotionEvent::Failed(error)
                }
            })
        }
        #[cfg(not(target_os = "linux"))]
        match self.unavailable {}
    }
}
impl PromotionReport {
    pub fn cancelled(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.cancelled
        }
        #[cfg(not(target_os = "linux"))]
        {
            match self.unavailable {}
        }
    }
    pub fn complete(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.complete()
        }
        #[cfg(not(target_os = "linux"))]
        {
            match self.unavailable {}
        }
    }
    pub fn summary(&self, review: bool) -> std::io::Result<Value> {
        #[cfg(target_os = "linux")]
        {
            self.inner.summary(review)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = review;
            match self.unavailable {}
        }
    }
    pub fn result_page(
        &self,
        category: &str,
        offset: usize,
        limit: usize,
        review: bool,
    ) -> std::io::Result<Value> {
        #[cfg(target_os = "linux")]
        {
            self.inner.result_page(category, offset, limit, review)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (category, offset, limit, review);
            match self.unavailable {}
        }
    }
}
