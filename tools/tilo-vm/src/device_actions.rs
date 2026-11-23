//! The device's own actions, asked as action frames on the owner-authenticated route.

use tilo_vm_protocol::DeviceActions;

use crate::scenario::console::{Confirmation, catalogue_frame, invoke_frame};
use crate::{cli::DeviceActionCommand, error::CliError};

/// The frame a command asks; private arguments are read only from stdin.
pub fn request(
    command: DeviceActionCommand,
    reader: impl std::io::Read,
) -> Result<DeviceActions, CliError> {
    let request = match command {
        DeviceActionCommand::List { query } => catalogue_frame(&query.unwrap_or_default()),
        DeviceActionCommand::Invoke {
            provider,
            id,
            confirm,
            args_stdin: _,
        } => {
            let (action, resource) = id.split_once('@').ok_or_else(|| {
                CliError::InvalidInput(format!("{id}: name the action as ACTION@RESOURCE"))
            })?;
            let confirmation = confirm.then_some(Confirmation::Confirmed);
            let args = private_action_input(reader)?;
            invoke_frame(&provider, resource, action, args, confirmation.as_ref()).0
        }
    };
    Ok(DeviceActions {
        line: request.to_string(),
    })
}

pub(crate) fn private_action_input(
    reader: impl std::io::Read,
) -> Result<serde_json::Map<String, serde_json::Value>, CliError> {
    use std::io::Read as _;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    reader
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| CliError::InvalidInput("could not read private action arguments".into()))?;
    if bytes.len() > 65536 {
        return Err(CliError::InvalidInput(
            "private action arguments exceed 64 KiB".into(),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        CliError::InvalidInput("private action arguments must be a JSON object".into())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn private_action_stdin_is_bounded_and_never_echoes_invalid_credentials() {
        let valid =
            br#"{"passphrase":"private-fixture-answer","networks":[],"allow_unsealed":false}"#;
        assert!(super::private_action_input(&valid[..]).is_ok());
        for invalid in [
            br#""private-fixture-answer""#.to_vec(),
            br#"{"passphrase":"private-fixture-answer""#.to_vec(),
            vec![b'x'; 65537],
        ] {
            let error = super::private_action_input(&invalid[..])
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private-fixture-answer"));
        }
    }
}
