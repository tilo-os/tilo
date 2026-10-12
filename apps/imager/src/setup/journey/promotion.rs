//! The same reviewed worker serves semantic actions and interactive callbacks.
#[cfg(target_os = "linux")]
use super::super::home_transfer::promotion::{PromotionEvent, PromotionReview, PromotionWorker};
#[derive(Default)]
pub(super) struct Promotion {
    #[cfg(target_os = "linux")]
    _source: Option<super::super::user_transfer::TransferReview>,
    #[cfg(target_os = "linux")]
    reviewing: Option<std::sync::mpsc::Receiver<Result<PromotionReview, String>>>,
    #[cfg(target_os = "linux")]
    review: Option<PromotionReview>,
    #[cfg(target_os = "linux")]
    worker: Option<PromotionWorker>,
    #[cfg(target_os = "linux")]
    report: Option<super::super::home_transfer::promotion::PromotionReport>,
    #[cfg(target_os = "linux")]
    report_review: bool,
    attempted: bool,
    state: &'static str,
    result: serde_json::Value,
}
impl Promotion {
    #[cfg(target_os = "linux")]
    pub fn review(
        &mut self,
        source: super::super::user_transfer::TransferReview,
        home: std::path::PathBuf,
        journal: std::path::PathBuf,
    ) -> Result<(), &'static str> {
        if self.busy() || self.attempted {
            return Err("installer-preview-transfer-busy");
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        self._source = Some(source.clone());
        self.review = None;
        self.report = None;
        self.reviewing = Some(receiver);
        self.state = "reviewing";
        self.result = serde_json::Value::Null;
        std::thread::spawn(move || {
            let _ = sender.send(source.review_promotion(&home, &journal));
        });
        Ok(())
    }
    pub fn start(&mut self, retry: bool) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        {
            if self.busy() {
                return Err("installer-preview-transfer-busy");
            }
            if self.attempted != retry {
                return Err("installer-preview-transfer-not-reviewed");
            }
            let review = self
                .review
                .as_ref()
                .ok_or("installer-preview-transfer-not-reviewed")?;
            self.worker = Some(
                review
                    .start(retry)
                    .map_err(|_| "installer-preview-transfer-start-failed")?,
            );
            self.report = None;
            self.attempted = true;
            self.state = "running";
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = retry;
            Err("installer-preview-import-unavailable")
        }
    }
    pub fn cancel(&mut self) -> Result<(), &'static str> {
        #[cfg(target_os = "linux")]
        if let Some(worker) = &self.worker {
            worker.cancel();
            self.state = "cancelling";
            return Ok(());
        }
        Err("installer-preview-transfer-not-running")
    }
    pub fn busy(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.reviewing.is_some() || self.worker.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
    pub fn poll(&mut self) -> bool {
        #[cfg(target_os = "linux")]
        {
            let mut changed = false;
            if let Some(receiver) = &self.reviewing {
                let result = match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some(Err("Home adoption review stopped".into()))
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                };
                if let Some(result) = result {
                    self.reviewing = None;
                    match result {
                        Ok(review) => {
                            match review.preview() {
                                Ok(report) => match report.summary(true) {
                                    Ok(value) => {
                                        self.result = value;
                                        self.report = Some(report);
                                        self.report_review = true;
                                    }
                                    Err(error) => {
                                        self.state = "failed";
                                        self.result =
                                            serde_json::json!({"error":error.to_string()});
                                        changed = true;
                                        return changed;
                                    }
                                },
                                Err(error) => {
                                    self.state = "failed";
                                    self.result = serde_json::json!({"error":error});
                                    changed = true;
                                    return changed;
                                }
                            }
                            self.review = Some(review);
                            self.state = "reviewed";
                        }
                        Err(error) => {
                            self.state = "failed";
                            self.result = serde_json::json!({"error":error});
                        }
                    }
                    changed = true;
                }
            }
            if let Some(event) = self.worker.as_mut().and_then(PromotionWorker::try_next) {
                self.worker = None;
                match event {
                    PromotionEvent::Finished(report) => {
                        self.state = if report.cancelled {
                            "cancelled"
                        } else if report.complete() {
                            "complete"
                        } else {
                            "incomplete"
                        };
                        match report.summary(false) {
                            Ok(value) => {
                                self.result = value;
                                self.report = Some(report);
                                self.report_review = false;
                            }
                            Err(error) => {
                                self.state = "failed";
                                self.result = serde_json::json!({"error":error.to_string()});
                            }
                        }
                    }
                    PromotionEvent::Failed(error) => {
                        self.state = "failed";
                        self.result = serde_json::json!({"error":error});
                    }
                }
                changed = true;
            }
            changed
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
    pub fn page(
        &self,
        category: &str,
        offset: u64,
        limit: u64,
    ) -> Result<serde_json::Value, &'static str> {
        #[cfg(target_os = "linux")]
        {
            let report = self
                .report
                .as_ref()
                .ok_or("installer-preview-transfer-not-reviewed")?;
            let offset =
                usize::try_from(offset).map_err(|_| "installer-preview-transfer-not-reviewed")?;
            let limit =
                usize::try_from(limit).map_err(|_| "installer-preview-transfer-not-reviewed")?;
            report
                .result_page(category, offset, limit, self.report_review)
                .map_err(|_| "installer-preview-transfer-not-reviewed")
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (category, offset, limit);
            Err("installer-preview-import-unavailable")
        }
    }
    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({"state":if self.state.is_empty(){"idle"}else{self.state},"result":self.result})
    }
}
