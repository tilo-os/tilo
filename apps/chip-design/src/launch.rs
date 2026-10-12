//! Reports arrive as files lent by the existing host launch/content handoff.
use crate::model::{MAX_REPORT_BYTES, StorageReport, report_id};
use crate::resource::{Project as ChipProject, import};
use crate::verification::VerificationRecord;
use crate::view::ChipEffect;
use ui::framework::LaunchInput;
use ui::tilo_resource::Invoke;

fn report(input: &LaunchInput<'_>) -> Result<Option<(Invoke<ChipProject>, String)>, String> {
    let Some(file) = input.file() else {
        return Ok(None);
    };
    if file.bytes().len() > MAX_REPORT_BYTES {
        return Err("report exceeds the 1 MiB import limit".into());
    }
    let json = std::str::from_utf8(file.bytes()).map_err(|error| error.to_string())?;
    let action = import(json.to_owned())?;
    let identity = if StorageReport::parse(json).is_ok() {
        report_id(json)
    } else {
        VerificationRecord::import(json)?.0
    };
    Ok(Some((action, identity)))
}

pub fn invalid(input: &LaunchInput<'_>) -> bool {
    report(input).is_err()
}

pub fn actions(input: &LaunchInput<'_>) -> Vec<Invoke<ChipProject>> {
    let Ok(Some((action, identity))) = report(input) else {
        return Vec::new();
    };
    let inspect = Invoke::effect(&ChipEffect::Inspect {
        report: Some(identity),
    });
    vec![action, inspect]
}
