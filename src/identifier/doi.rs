use biblatex::{Bibliography, Entry};
use once_cell::sync::Lazy;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use regex::Regex;
use url::Url;

use crate::{identifier::Identifier, resolver::IdFamily};
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

pub struct Doi<'a> {
    _name: &'a str,
    prefix: &'a str,
    suffix: &'a str,
}

impl<'a> Identifier<'a> for Doi<'a> {
    fn parse(identifier: &'a str) -> Option<Box<Self>> {
        let mut s = identifier.trim();

        // Normalise common textual prefixes.
        if let Some(rest) = s
            .strip_prefix("doi:")
            .or_else(|| s.strip_prefix("DOI:"))
            .or_else(|| s.strip_prefix("urn:doi:"))
            .or_else(|| s.strip_prefix("URN:DOI:"))
        {
            s = rest.trim_start();
        }

        // Drop query string or fragment early if present.
        if let Some(idx) = s.find(['?', '#']) {
            s = &s[..idx];
        }

        // Trim trailing punctuation commonly found in prose.
        s = s.trim_end_matches(|c: char| {
            matches!(c, '.' | ',' | ';' | ':' | ')' | ']' | '}' | '\"' | '\'')
        });

        // Key change: find a DOI anywhere, not just when the whole string is a DOI.
        // Case-insensitive, based on Crossref guidance.
        static DOI_ANYWHERE_RE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"(?i)\b(10\.\d{4,9})/([-._;()/:A-Z0-9]+)\b").unwrap());

        let caps = DOI_ANYWHERE_RE.captures(s)?;

        let name = caps.get(0)?.as_str(); // full DOI
        let prefix = caps.get(1)?.as_str();
        let suffix = caps.get(2)?.as_str();

        Some(Box::new(Doi {
            _name: name,
            prefix,
            suffix,
        }))
    }

    fn resolve(&self) -> anyhow::Result<Entry> {
        let url = self.to_url();
        let body: String = ureq::get(url.as_str())
            .header("Accept", "application/x-bibtex")
            .header(
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.10 Safari/605.1.1",
            )
            .call()?
            .body_mut()
            .read_to_string()?;

        let bib = Bibliography::parse(&body)
            .map_err(|e| anyhow::anyhow!("failed to parse BibLaTeX: {e}"))?;
        let res = bib
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("empty bibliography"))?;
        Ok(res)
    }
}

impl<'a> Doi<'a> {
    fn to_url(&self) -> Url {
        let enc_suffix = utf8_percent_encode(self.suffix, PATH_SEGMENT_ENCODE_SET).to_string();
        Url::parse(format!("https://doi.org/{}/{}", self.prefix, enc_suffix).as_str()).unwrap()
    }
}

impl IdFamily for Doi<'_> {
    type For<'a> = Doi<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;
    // Avoid importing the full prelude to keep clippy happy; use fully qualified macros instead.
    use proptest::strategy::Strategy;

    // Allowed characters in DOI suffix per our regex: [-._;()/:A-Z0-9]+ (case-insensitive)
    fn doi_suffix_char() -> impl Strategy<Value = char> {
        // Letters (both cases), digits, and specific punctuation
        let uppers = proptest::sample::select(('A'..='Z').collect::<Vec<_>>());
        let lowers = proptest::sample::select(('a'..='z').collect::<Vec<_>>());
        let digits = proptest::sample::select(('0'..='9').collect::<Vec<_>>());
        let punct = proptest::sample::select(vec!['-', '.', '_', ';', '(', ')', '/', ':']);
        proptest::prop_oneof![uppers, lowers, digits, punct]
    }

    // Generate a valid DOI suffix that ends with an alphanumeric to satisfy the trailing \b.
    fn doi_suffix(min: usize, max: usize) -> impl Strategy<Value = String> {
        // Ensure last char is alphanumeric
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

    // Generate a valid DOI core like "10.12345/ABC-123"
    fn doi_core() -> impl Strategy<Value = (String, String, String)> {
        (
            // 4-9 digits after "10."
            proptest::collection::vec(
                proptest::sample::select(('0'..='9').collect::<Vec<_>>()),
                4..=9,
            )
            .prop_map(|v| v.into_iter().collect::<String>()),
            doi_suffix(1, 128),
        )
            .prop_map(|(digits, suffix)| {
                let prefix = format!("10.{digits}");
                let full = format!("{}/{}", prefix, suffix);
                (full, prefix, suffix)
            })
    }

    // Normalize dot-segments per RFC 3986 section 5.2.4 for testing URL path equivalence.
    fn remove_dot_segments(path: &str) -> String {
        let input = path.split('/').collect::<Vec<_>>();
        let mut output: Vec<&str> = Vec::new();
        let mut i = 0;
        while i < input.len() {
            let seg = input[i];
            if seg == "." {
                // Skip current segment
            } else if seg == ".." {
                if !output.is_empty() {
                    output.pop();
                }
            } else {
                output.push(seg);
            }
            i += 1;
        }
        output.join("/")
    }

    // For any generated DOI core, our parser should find it and capture components.
    #[test]
    fn parse_finds_generated_doi() {
        proptest::proptest!(|(t in doi_core())| {
            let (full, prefix, suffix) = t;
            let d: Box<Doi<'_>> = <Doi<'_> as Identifier<'_>>::parse(&full).expect("should parse");
            proptest::prop_assert_eq!(d.prefix, prefix);
            proptest::prop_assert_eq!(d.suffix, suffix);
        })
    }

    // DOI can be embedded in text with optional trailing punctuation; parser should still find it.
    #[test]
    fn parse_finds_embedded_doi() {
        let trails = vec![".", ",", ";", ":", ")", "]", "}", "\"", "'"];
        proptest::proptest!(|(t in doi_core(), trail in proptest::option::of(proptest::sample::select(trails.clone())), lead_ws in "[ \t]*", tail_ws in "[ \t]*")| {
            let (full, _p, _s) = t;
            let mut decorated = format!("{}{}{}", lead_ws, full, tail_ws);
            if let Some(t) = trail { decorated.push_str(t); }
            let wrapped = format!("https://doi.org/{}", full);
            for candidate in [decorated.as_str(), wrapped.as_str()] {
                let _d: Box<Doi<'_>> = <Doi<'_> as Identifier<'_>>::parse(candidate).expect("should parse when embedded");
            }
        })
    }

    // to_url builds the URL with scheme/host set and normalized path segments
    #[test]
    fn to_url_roundtrips_suffix() {
        proptest::proptest!(|(t in doi_core())| {
            let (_full, prefix, suffix) = t;
            let s = format!("{}/{}", prefix, suffix);
            let doi: Box<Doi<'_>> = <Doi<'_> as Identifier<'_>>::parse(&s).expect("should parse");
            let url = doi.to_url();
            assert_eq!(url.scheme(), "https");
            assert_eq!(url.domain(), Some("doi.org"));
            // Reconstruct the suffix joined across path segments and compare to normalized input
            let mut segs = url.path_segments().expect("url has segments");
            let _first = segs.next().unwrap();
            let rest: Vec<_> = segs.collect();
            assert_eq!(rest.join("/"), remove_dot_segments(&suffix));
        })
    }

    // If multiple DOIs exist in a string, parse should return the first match
    #[test]
    fn parse_finds_first_of_multiple_dois() {
        proptest::proptest!(|(t1 in doi_core(), t2 in doi_core(), lead in "[A-Za-z0-9 _.,;:()/-]{0,10}", tail in "[A-Za-z0-9 _.,;:()/-]{0,10}")| {
            let (full1, prefix1, suffix1) = t1;
            let (full2, _prefix2, _suffix2) = t2;
            // Ensure non-word boundaries around DOIs to satisfy the regex \b
            let s = format!("{} {} {} {}", lead, full1, full2, tail);
            let d: Box<Doi<'_>> = <Doi<'_> as Identifier<'_>>::parse(&s).expect("should parse first DOI");
            proptest::prop_assert_eq!(d.prefix, prefix1);
            proptest::prop_assert_eq!(d.suffix, suffix1);
        })
    }

    // Parser should handle textual prefixes and optional whitespace
    #[test]
    fn parse_with_textual_prefixes() {
        let prefixes = vec!["doi:", "DOI:", "urn:doi:", "URN:DOI:"];
        proptest::proptest!(|(t in doi_core(), pre in proptest::sample::select(prefixes.clone()), ws in "[ \t]*")| {
            let (full, prefix, suffix) = t;
            let decorated = format!("{}{}{}", pre, ws, full);
            let d: Box<Doi<'_>> = <Doi<'_> as Identifier<'_>>::parse(&decorated).expect("should parse with textual prefix");
            proptest::prop_assert_eq!(d.prefix, prefix.clone());
            proptest::prop_assert_eq!(d.suffix, suffix.clone());
        })
    }

    // Parser should ignore query strings and fragments when present
    #[test]
    fn parse_handles_query_and_fragment() {
        proptest::proptest!(|(t in doi_core(), q in "[A-Za-z0-9=&_-]{0,16}", f in "[A-Za-z0-9_-]{0,16}")| {
            let (full, prefix, suffix) = t;
            let mut s = format!("{}?{}#{}", full, q, f);
            let d: Box<Doi<'_>> = <Doi<'_> as Identifier<'_>>::parse(&s).expect("should parse despite query/fragment");
            proptest::prop_assert_eq!(d.prefix, prefix.clone());
            proptest::prop_assert_eq!(d.suffix, suffix.clone());

            s = format!("https://doi.org/{}?{}#{}", full, q, f);
            let d: Box<Doi<'_>> = <Doi<'_> as Identifier<'_>>::parse(&s).expect("should parse doi.org URL with query/fragment");
            proptest::prop_assert_eq!(d.prefix, prefix);
            proptest::prop_assert_eq!(d.suffix, suffix);
        })
    }

    // Non-DOI strings should not parse
    #[test]
    fn parse_rejects_non_doi() {
        proptest::proptest!(|(s in "[A-Za-z0-9 _-]{1,64}")| {
            proptest::prop_assume!(!s.contains("10."));
            assert!(<Doi<'_> as Identifier<'_>>::parse(&s).is_none());
        })
    }
}
