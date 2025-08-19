use biblatex::Entry;

pub mod doi;

pub trait Identifier<'a>: 'a {
    fn parse(identifier: &'a str) -> Option<Box<Self>>
    where
        Self: Sized;
    fn resolve(&self) -> anyhow::Result<Entry>;
}
