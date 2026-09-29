use std::process::ExitCode;

fn main() -> ExitCode {
    match termr::run(std::env::args_os()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("termr: {error:#}");
            ExitCode::FAILURE
        }
    }
}
