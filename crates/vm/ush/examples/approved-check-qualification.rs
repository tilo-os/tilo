//! Developer-only production supervision probe. Never an agent shell gateway.
use std::cell::RefCell;
use std::io::Write as _;
use std::path::Path;
fn main() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let reports = args.last().is_some_and(|arg| arg == "--reports");
    if reports {
        args.pop();
    }
    if !matches!(args.len(), 2 | 3) {
        return Err(
            "usage: approved-check-qualification PRIVATE_CONFIG NEW_RECEIPT [CANCEL_AFTER_MS] [--reports]"
                .into(),
        );
    }
    let config = args.first().ok_or("missing private config")?;
    let receipt = args.get(1).ok_or("missing new receipt")?;
    let cancel = args
        .get(2)
        .map(|s| s.parse::<u64>().map_err(|e| e.to_string()))
        .transpose()?;
    let report_failure = RefCell::new(None);
    let result = ush::approved_checks::developer_probe_with_reports(
        Path::new(config),
        Path::new(receipt),
        cancel,
        |event| {
            if reports {
                let serialized = match serde_json::to_string(&event) {
                    Ok(serialized) => serialized,
                    Err(error) => {
                        *report_failure.borrow_mut() = Some(error.to_string());
                        return;
                    }
                };
                let mut output = std::io::stdout().lock();
                if let Err(error) = writeln!(output, "{serialized}").and_then(|()| output.flush()) {
                    *report_failure.borrow_mut() = Some(error.to_string());
                }
            }
        },
    )?;
    if let Some(error) = report_failure.into_inner() {
        return Err(error);
    }
    if !result.cleanup_observed
        || !matches!(result.outcome, ush::approved_checks::CheckOutcome::Exited)
        || result.exit_code != Some(0)
    {
        return Err(format!(
            "owned check was not successful: {:?}, {:?}; inspect its receipt",
            result.outcome, result.failure
        ));
    }
    Ok(())
}
