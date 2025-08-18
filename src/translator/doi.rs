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
        let mut identifier = identifier.trim();

        if let Some(rest) = identifier
            .strip_prefix("doi:")
            .or_else(|| identifier.strip_prefix("DOI:"))
            .or_else(|| identifier.strip_prefix("urn:doi:"))
            .or_else(|| identifier.strip_prefix("URN:DOI:"))
        {
            identifier = rest.trim_start();
        }

        if let Some(rest) = identifier
            .strip_prefix("https://doi.org/")
            .or_else(|| identifier.strip_prefix("http://doi.org/"))
            .or_else(|| identifier.strip_prefix("https://dx.doi.org/"))
            .or_else(|| identifier.strip_prefix("http://dx.doi.org/"))
        {
            identifier = rest;
        }

        if let Some(idx) = identifier.find(['?', '#']) {
            identifier = &identifier[..idx];
        }

        identifier = identifier.trim_end_matches(|c: char| {
            matches!(c, '.' | ',' | ';' | ':' | ')' | ']' | '}' | '\"' | '\'')
        });

        static DOI_RE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"^(10\.\d{4,}(?:\.\d+)*)(/([^\p{C}]+))$").unwrap());

        let caps = DOI_RE.captures(identifier)?;

        let name = caps.get(0)?.as_str();
        let prefix = caps.get(1)?.as_str();
        let suffix = caps.get(3)?.as_str();

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
