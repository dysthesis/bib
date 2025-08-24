use anyhow::anyhow;
use biblatex::Entry;

use crate::identifier::{Identifier, arxiv::Arxiv, doi::Doi, embedded::Embedded, usenix::Usenix};

type ParserFn = for<'a> fn(&'a str) -> Option<Box<dyn Identifier<'a> + 'a>>;

/// List of parsers to iterate over.
///
/// NOTE: Ordering is important here, as it signifies priority. If two parsers are able to parse a
/// given identifier, the first one to show up in this list will be used.
static PARSERS: &[ParserFn] = &[
    erase::<Doi>(),
    erase::<Arxiv>(),
    // More specific before generic embedded translator
    erase::<Usenix>(),
    erase::<Embedded>(),
];

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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::Strategy;
    use regex::Regex;

    // DOI-like generators (kept in sync with identifier::doi tests)
    fn doi_suffix_char() -> impl Strategy<Value = char> {
        let uppers = proptest::sample::select(('A'..='Z').collect::<Vec<_>>());
        let lowers = proptest::sample::select(('a'..='z').collect::<Vec<_>>());
        let digits = proptest::sample::select(('0'..='9').collect::<Vec<_>>());
        let punct = proptest::sample::select(vec!['-', '.', '_', ';', '(', ')', '/', ':']);
        proptest::prop_oneof![uppers, lowers, digits, punct]
    }

    fn doi_suffix(min: usize, max: usize) -> impl Strategy<Value = String> {
        let last = proptest::prop_oneof![
            proptest::sample::select(('A'..='Z').collect::<Vec<_>>()),
            proptest::sample::select(('a'..='z').collect::<Vec<_>>()),
            proptest::sample::select(('0'..='9').collect::<Vec<_>>()),
        ];
        (min..=max).prop_flat_map(move |len| {
            if len == 1 {
                last.clone().prop_map(|c| c.to_string()).boxed()
            } else {
                (
                    proptest::collection::vec(doi_suffix_char(), len - 1),
                    last.clone(),
                )
                    .prop_map(|(mut v, last)| {
                        v.push(last);
                        v.into_iter().collect::<String>()
                    })
                    .boxed()
            }
        })
    }

    fn doi_core() -> impl Strategy<Value = String> {
        (
            proptest::collection::vec(
                proptest::sample::select(('0'..='9').collect::<Vec<_>>()),
                4..=9,
            )
            .prop_map(|v| v.into_iter().collect::<String>()),
            doi_suffix(1, 64),
        )
            .prop_map(|(digits, suffix)| format!("10.{}/{}", digits, suffix))
    }

    #[test]
    fn resolver_parse_accepts_generated_doi() {
        proptest::proptest!(|(full in doi_core())| {
            proptest::prop_assert!(parse(&full).is_some());
        })
    }

    #[test]
    fn resolver_parse_rejects_non_doi() {
        let re = Regex::new(r"(?i)\b(10\.\d{4,9})/([-._;()/:A-Z0-9]+)\b").unwrap();
        proptest::proptest!(|(s in "[A-Za-z0-9 _-]{1,64}")| {
            proptest::prop_assume!(!re.is_match(&s));
            proptest::prop_assert!(parse(&s).is_none());
        })
    }

    #[test]
    fn resolver_resolve_errors_for_non_doi() {
        let re = Regex::new(r"(?i)\b(10\.\d{4,9})/([-._;()/:A-Z0-9]+)\b").unwrap();
        proptest::proptest!(|(s in "[A-Za-z0-9 _-]{1,64}")| {
            proptest::prop_assume!(!re.is_match(&s));
            let err = resolve(&s).unwrap_err();
            proptest::prop_assert!(err.to_string().contains("unrecognised identifier"));
        })
    }
}
