use std::ffi::OsString;

use anyhow::Result;

pub mod cli;
pub mod config;
mod tui;

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    match cli::parse_args(args)? {
        cli::Command::Run => {
            let paths = config::AppPaths::discover()?;
            let _config = config::AppConfig::load(&paths.config_file)?;
            tui::run()
        }
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
