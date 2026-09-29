use std::ffi::OsString;

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Help,
    Version,
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command> {
    let mut args = args.into_iter();
    let _program = args.next();

    let command = match args.next().as_deref() {
        None => Command::Run,
        Some(value) if value == "--help" || value == "-h" => Command::Help,
        Some(value) if value == "--version" || value == "-V" => Command::Version,
        Some(value) => bail!(
            "unknown argument: {}\n\n{}",
            value.to_string_lossy(),
            help()
        ),
    };

    if let Some(value) = args.next() {
        bail!(
            "unexpected argument: {}\n\n{}",
            value.to_string_lossy(),
            help()
        );
    }

    Ok(command)
}

pub fn help() -> &'static str {
    concat!(
        "Termr - terminal-first SSH troubleshooting\n\n",
        "Usage:\n",
        "  termr             Open the terminal interface\n",
        "  termr --help      Show this help\n",
        "  termr --version   Show the installed version\n"
    )
}
