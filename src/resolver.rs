use anyhow::anyhow;
use biblatex::Entry;

use crate::identifier::{Identifier, doi::Doi};

type ParserFn = for<'a> fn(&'a str) -> Option<Box<dyn Identifier<'a> + 'a>>;

/// List of parsers to iterate over.
///
/// NOTE: Ordering is important here, as it signifies priority. If two parsers are able to parse a
/// given identifier, the first one to show up in this list will be used.
static PARSERS: &[ParserFn] = &[erase::<Doi>()];

// Use GAT because we don't have higher-kinded types in Rust (sad)
pub trait IdFamily {
    type For<'a>: Identifier<'a>;
}

/// Get the parser method of a given identifier `F` and erase its type to make the compiler happy.
const fn erase<F: IdFamily>() -> ParserFn {
    // A generic function item, still polymorphic in 'a.
    fn call<'a, G: IdFamily>(s: &'a str) -> Option<Box<dyn Identifier<'a> + 'a>> {
        // Perform type erasure to make the compiler happy. Basically, we're telling it that we
        // don't care what the actual type of `G` is; treat it as an `Identifier` instead, and get
        // the `parse` method from it.
        <G::For<'a> as Identifier<'a>>::parse(s)
            // Then, we again erase the type of the resulting `Identifier`, because again, we don't
            // care what the concrete type is. All we care about is that you can `parse` an
            // identifier with it, and then `resolve` the metadata accordingly.
            .map(|x| x as Box<dyn Identifier<'a> + 'a>)
    }

    // Force coercion to the HRTB fn-pointer type.
    let f: ParserFn = call::<F>;
    f
}

/// Guess what type `identifier` is
pub fn parse<'a>(identifier: &'a str) -> Option<Box<dyn Identifier<'a> + 'a>> {
    PARSERS.iter().find_map(|f| f(identifier))
}

/// Guess what type `iderntifier` is and resolve the metadata.
pub fn resolve(identifier: &str) -> anyhow::Result<Entry> {
    parse(identifier)
        .map(|e| e.resolve())
        .ok_or_else(|| anyhow!("unrecognised identifier: {identifier}"))?
}
