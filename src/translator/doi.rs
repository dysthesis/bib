use biblatex::{Bibliography, Entry};
use once_cell::sync::Lazy;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use regex::Regex;
use url::Url;

use crate::translator::Translator;
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

pub struct DoiTranslator<'a> {
    name: &'a str,
    prefix: &'a str,
    suffix: &'a str,
}

impl<'a> Translator<'a> for DoiTranslator<'a> {
    fn parse(identifier: &'a str) -> Option<Self> {
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

        Some(DoiTranslator {
            name,
            prefix,
            suffix,
        })
    }

    fn resolve(&self) -> anyhow::Result<Entry> {
        let url = self.to_url();
        let body: String = ureq::get(url.as_str())
            .header("Accept", "application/x-bibtex")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.10 Safari/605.1.1")
            .call()?
            .body_mut()
            .read_to_string()?;

        let bib = Bibliography::parse(&body).unwrap();
        let res = bib.iter().next().unwrap().clone();
        Ok(res)
    }
}

impl<'a> DoiTranslator<'a> {
    fn to_url(&self) -> Url {
        let enc_suffix = utf8_percent_encode(self.suffix, PATH_SEGMENT_ENCODE_SET).to_string();
        Url::parse(format!("https://doi.org/{}/{}", self.prefix, enc_suffix).as_str()).unwrap()
    }
}
