use std::ffi::OsString;

use termr::cli::{Command, parse_args};

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn no_arguments_start_the_tui() {
    assert_eq!(parse_args(args(&["termr"])).unwrap(), Command::Run);
}

#[test]
fn help_flags_return_help_without_starting_the_tui() {
    assert_eq!(
        parse_args(args(&["termr", "--help"])).unwrap(),
        Command::Help
    );
    assert_eq!(parse_args(args(&["termr", "-h"])).unwrap(), Command::Help);
}

#[test]
fn version_flags_return_version_without_starting_the_tui() {
    assert_eq!(
        parse_args(args(&["termr", "--version"])).unwrap(),
        Command::Version
    );
    assert_eq!(
        parse_args(args(&["termr", "-V"])).unwrap(),
        Command::Version
    );
}

#[test]
fn unsupported_arguments_are_rejected() {
    let error = parse_args(args(&["termr", "--unknown"])).unwrap_err();

    assert!(error.to_string().contains("unknown argument"));
}
