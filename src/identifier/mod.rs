use biblatex::Entry;

pub mod doi;

pub trait Identifier<'a>: Sized + 'a {
    fn parse(identifier: &'a str) -> Option<Self>;
    fn resolve(&self) -> anyhow::Result<Entry>;
}
