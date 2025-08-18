use std::{fs, path::PathBuf, str::FromStr};

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Fetch the information on one or more citation items
    Fetch {
        #[arg(value_name = "SRC")]
        from: Vec<Source>,
    },
    /// Pull the files related to the given citation items
    Pull {
        #[arg(value_name = "SRC")]
        from: Vec<Source>,
    },
}

#[derive(Clone, Debug)]
/// Defines where we can get citation items from, which can either be
///
/// - a single identifier, or
/// - a bibliography file.
///
/// The latter will be treated as a list of the former.
pub enum Source {
    Identifier(String),
    File(PathBuf),
}

impl FromStr for Source {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // NOTE: We don't deal with validation in the CLI parsing layer just yet. We just try to
        // guess if it's an identifier or a file. Later, we'll deal with not only checking that a
        // file is an actual file, but also figuring out if it's BibTeX or Hayagriva, parsing that
        // into a list of items, and then also parsing a single identifier into a citation item,
        // thus aking them uniform.

        // Is this a path?
        if let Ok(path) = fs::canonicalize(s) {
            Ok(Source::File(path))
        }
        // No? Must be an identifier then!
        else {
            Ok(Source::Identifier(s.to_string()))
        }
    }
}
