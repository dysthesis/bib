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
            if let Some(Source::Identifier(id)) = from.first() {
                let translator = DoiTranslator::parse(&id).unwrap();
                let res = translator.resolve().unwrap().to_biblatex_string();
                println!("{res}")
            }
        }
        cli::Command::Pull { from } => todo!(),
    }
    Ok(())
}
