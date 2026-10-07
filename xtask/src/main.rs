use std::{fs, path::PathBuf};

use clap::{CommandFactory, Parser, Subcommand};

mod live_read;

#[derive(Parser)]
struct Xtask {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record read-only acceptance evidence from the installed CLI.
    LiveRead(live_read::Args),
    /// Generate instantctl manual pages from the CLI definitions.
    Man {
        #[arg(long, default_value = "target/man")]
        output: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let Xtask { command } = Xtask::parse();
    match command {
        Command::LiveRead(args) => live_read::run(args)?,
        Command::Man { output } => {
            fs::create_dir_all(&output)?;
            render_man_pages(instantctl::cli::Cli::command(), "instantctl", &output)?;
        }
    }
    Ok(())
}

fn render_man_pages(
    command: clap::Command,
    name: &str,
    output: &std::path::Path,
) -> anyhow::Result<()> {
    let mut command = command.name(name.to_owned());
    command.build();
    for subcommand in command.get_subcommands() {
        render_man_pages(
            subcommand.clone(),
            &format!("{name}-{}", subcommand.get_name()),
            output,
        )?;
    }
    let mut page = fs::File::create(output.join(format!("{name}.1")))?;
    clap_mangen::Man::new(command).render(&mut page)?;
    Ok(())
}
