mod cli;
mod collector;
mod graph;
mod metrics;
mod status;

use lantern::model;

use std::{
    error::Error,
    io::{self, Write},
    process::ExitCode,
};

use cli::Command;

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "lantern: {error}\nTry 'lantern --help' for usage."
            );
            return ExitCode::from(2);
        }
    };

    if let Err(error) = run(command) {
        // Consumers such as `head` may deliberately close the output early.
        if command != Command::Collect
            && error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
        {
            return ExitCode::SUCCESS;
        }
        let operation = match command {
            Command::Collect => "collector failed",
            Command::Graph => "cannot show history",
            Command::Snapshot => "cannot show snapshot",
            Command::Status => "cannot show status",
            _ => "cannot write output",
        };
        let _ = writeln!(io::stderr().lock(), "lantern: {operation}: {error}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn run(command: Command) -> Result<(), Box<dyn Error>> {
    if command == Command::Collect {
        return collector::run();
    }
    let mut output = io::stdout().lock();
    match command {
        Command::Help => output.write_all(cli::HELP.as_bytes())?,
        Command::Version => writeln!(output, "lantern {}", env!("CARGO_PKG_VERSION"))?,
        Command::Snapshot => cli::write_snapshot(
            &mut output,
            &metrics::collect_snapshot()?,
            &metrics::system::read_machine_info(),
        )?,
        Command::Graph => graph::run(&mut output)?,
        Command::Status => status::run(&mut output)?,
        Command::Collect => unreachable!("collector handled before locking stdout"),
    }
    output.flush()?;
    Ok(())
}
