use clap::Parser;

use crate::{
    cli::{Cli, Source},
    translator::{Translator, doi::DoiTranslator},
};

mod cli;
mod translator;

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match args.command {
        cli::Command::Fetch { from } => {
            if let Some(Source::Identifier(id)) = from.first()
                && let Some(translator) = DoiTranslator::parse(id)
                && let Ok(res) = translator.resolve()
            {
                println!("{}", res.to_biblatex_string())
            }
        }
        cli::Command::Pull { from } => todo!(),
    }
    Ok(())
}
