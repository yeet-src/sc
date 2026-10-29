use clap::CommandFactory;

use super::Result;

/// `sc completions <shell>`: print a completion script.
#[derive(clap::Args)]
pub struct Args {
    #[arg(value_enum)]
    shell: clap_complete::Shell,
}

pub fn run(a: Args) -> Result {
    use std::io::Write;
    let mut buf = Vec::new();
    clap_complete::generate(a.shell, &mut super::Cli::command(), "sc", &mut buf);
    let _ = std::io::stdout().write_all(&buf); // a closed pipe is not an error here
    Ok(())
}
