//! The `codoseo` command line: parses arguments, runs the command and turns the outcome
//! into an exit code (0 ok, 1 `--fail-on` reached, 2 usage or runtime error).

mod cli;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let args = cli::Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: could not start the async runtime: {e}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(cli::run(args)) {
        Ok(code) => ExitCode::from(code),
        // Whoever read our output stopped early (`| head`): that is their choice, not a failure.
        Err(e) if e.is_broken_pipe() => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", cli::clean(&e.to_string()));
            ExitCode::from(2)
        }
    }
}
