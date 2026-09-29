use std::ffi::OsString;

use anyhow::Result;

pub mod cli;
mod tui;

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    match cli::parse_args(args)? {
        cli::Command::Run => tui::run(),
        cli::Command::Help => {
            println!("{}", cli::help());
            Ok(())
        }
        cli::Command::Version => {
            println!("termr {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}
