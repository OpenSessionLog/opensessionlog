use osl::cli::{run, Cli};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match Cli::parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = run(cli) {
        eprintln!("osl: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
