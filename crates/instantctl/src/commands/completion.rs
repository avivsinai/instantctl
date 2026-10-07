use std::io::Write;

use clap::{Args as ClapArgs, CommandFactory};
use clap_complete::Shell;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_enum)]
    pub shell: Shell,
}

pub fn run(args: Args, out: &mut impl Write) -> anyhow::Result<()> {
    // Generate into memory so clap_complete cannot panic on a closed stdout.
    let mut bytes = Vec::new();
    clap_complete::generate(
        args.shell,
        &mut crate::cli::Cli::command(),
        "instantctl",
        &mut bytes,
    );
    crate::output::write_bytes(out, &bytes)?;
    Ok(())
}
