use clap::Parser;

use crate::cli::Cli;

mod cli;

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    println!("{args:?}");
    Ok(())
}
