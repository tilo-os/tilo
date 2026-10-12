//! Developer-only production supervision probe. Never an agent shell gateway.
use std::path::Path;
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !matches!(args.len(), 2 | 3) {
        return Err(
            "usage: approved-check-qualification PRIVATE_CONFIG NEW_RECEIPT [CANCEL_AFTER_MS]"
                .into(),
        );
    }
    let config = args.first().ok_or("missing private config")?;
    let receipt = args.get(1).ok_or("missing new receipt")?;
    let cancel = args
        .get(2)
        .map(|s| s.parse::<u64>().map_err(|e| e.to_string()))
        .transpose()?;
    let result =
        ush::approved_checks::developer_probe(Path::new(config), Path::new(receipt), cancel)?;
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
