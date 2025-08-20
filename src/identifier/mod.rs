use biblatex::Entry;

pub mod arxiv;
pub mod doi;
pub mod embedded;

pub trait Identifier<'a>: 'a {
    fn parse(identifier: &'a str) -> Option<Box<Self>>
    where
        Self: Sized;
    fn resolve(&self) -> anyhow::Result<Entry>;
}
