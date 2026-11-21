//! `tilo status` for the device admin without sudo: the listing the deployment status socket
//! answers ([`tilo_deployment_status`]), printed with its own exit status.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use tilo_deployment_status::{Answer, AskError, Listing};

pub use tilo_deployment_status::socket;

pub fn ask(socket: &Path, json: bool) -> Result<Answer> {
    match tilo_deployment_status::ask(socket, if json { Listing::Json } else { Listing::Text }) {
        Ok(answer) => Ok(answer),
        Err(AskError::NotAnAdmin) => bail!(
            "what is on this machine is shown to its admins; \
             as another account, run `sudo tilo status`"
        ),
        Err(error) => Err(error.into()),
    }
}

pub fn print(answer: &Answer) -> ExitCode {
    if answer.code == 0 {
        print!("{}", answer.text);
        ExitCode::SUCCESS
    } else {
        eprint!("{}", answer.text);
        ExitCode::from(answer.code)
    }
}
