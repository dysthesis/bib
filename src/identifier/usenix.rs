use anyhow::Context;
use biblatex::{Bibliography, Entry};
use once_cell::sync::Lazy;
use regex::Regex;
use url::Url;

use crate::{identifier::Identifier, resolver::IdFamily};

/// USENIX presentation-page identifier (compatibility mode by default).
///
/// Behaviour: URL-pattern detector; fetches the page once; extracts metadata with JSON-LD →
/// Highwire → OG/DC precedence; maps to BibLaTeX, then post-processes `title` by stripping
/// unescaped braces repeatedly, finally unescaping `\{` and `\}`.
pub struct Usenix {
    url: Url,
    // For future extension; currently only compatibility mode is used.
    _corrected_mode: bool,
}

impl<'a> Identifier<'a> for Usenix {
    fn parse(identifier: &'a str) -> Option<Box<Self>> {
        static DETECT_RE: Lazy<Regex> = Lazy::new(|| {
            Regex::new(
                r"(?x)^                # anchored
                https://               # scheme
                www\.usenix\.org       # host
                /conference/           # path segment
                .*                     # any in-between
                /presentation          # presentation segment
                (?:[/?\#].*)?         # optional tail (query/fragment/path)
                $",
            )
            .unwrap()
        });

        if !DETECT_RE.is_match(identifier) {
            return None;
        }
        let url = Url::parse(identifier).ok()?;
        // Canonicalisation in compatibility mode is trivial since detector enforces canonical form.
        Some(Box::new(Usenix {
            url,
            _corrected_mode: false,
        }))
    }

    fn resolve(&self) -> anyhow::Result<Entry> {
        // 1) Fetch exactly once
        let (final_url, html) = fetch(self.url.clone())?;

        // 2) Collect metadata signals
        let meta = collect_meta(&html);
        let json_ld = collect_json_ld(&html);
        let title_tag = collect_title(&html);
        let og_site = meta_property(&meta, "og:site_name");

        // 3) Choose primary source (JSON-LD preferred if it provides a plausible type/name)
        let json_has_articleish = json_ld_types(&json_ld).iter().any(|t| {
            matches!(t.as_str(),
                "ScholarlyArticle" | "Article" | "CreativeWork" | "PresentationDigitalDocument")
        });

        // 4) Field extraction with precedence (JSON-LD → Highwire → OG/DC → fallbacks)
        // Title
        let mut title = json_name(&json_ld)
            .or_else(|| meta_value(&meta, "citation_title"))
            .or_else(|| meta_property(&meta, "og:title"))
            .or_else(|| title_tag.clone())
            .unwrap_or_else(|| final_url.as_str().to_string());
        title = normalize_ws(&title);
        if let Some(site) = og_site.as_deref() {
            title = strip_site_suffix(&title, site);
        }

        // Authors
        let mut authors = if json_has_articleish {
            json_authors(&json_ld).unwrap_or_default()
        } else {
            Vec::new()
        };
        if authors.is_empty() {
            extend_creators(&mut authors, &meta, "citation_author");
            extend_creators_split(&mut authors, &meta, "citation_authors");
        }
        if authors.is_empty() {
            // OG article:author, ignore URLs
            authors.extend(
                meta.iter()
                    .filter(|m| m.property.as_deref() == Some("article:author"))
                    .filter_map(|m| {
                        let v = m.content.trim();
                        if Url::parse(v).is_ok() || v.is_empty() {
                            None
                        } else {
                            Some(v.to_string())
                        }
                    }),
            );
        }
        dedup_in_place(&mut authors);

        // Date
        let date = json_date_published(&json_ld)
            .or_else(|| meta_value(&meta, "citation_publication_date"))
            .or_else(|| meta_value(&meta, "citation_cover_date"))
            .or_else(|| meta_value(&meta, "citation_date"))
            .or_else(|| meta_property(&meta, "article:published_time"))
            .and_then(|d| normalise_date(&d));

        // Container
        let booktitle = meta_value(&meta, "citation_conference_title")
            .or_else(|| json_is_part_of_name(&json_ld));
        let journaltitle = meta_value(&meta, "citation_journal_title");

        // Volume, issue, pages
        let volume = meta_value(&meta, "citation_volume");
        let number = meta_value(&meta, "citation_issue");
        let pages = build_pages(
            meta_value(&meta, "citation_firstpage"),
            meta_value(&meta, "citation_lastpage"),
        );

        // Identifiers
        let mut doi = meta_value(&meta, "citation_doi").and_then(clean_doi);
        let isbn = meta_value(&meta, "citation_isbn");

        // URL
        let url = json_url(&json_ld)
            .or_else(|| meta_value(&meta, "citation_public_url"))
            .or_else(|| meta_value(&meta, "citation_abstract_html_url"))
            .or_else(|| meta_value(&meta, "citation_fulltext_html_url"))
            .or_else(|| meta_property(&meta, "og:url"))
            .and_then(|u| absolutise(&final_url, &u).ok())
            .unwrap_or_else(|| final_url.clone());

        // Language
        let language = meta_value(&meta, "citation_language")
            .or_else(|| meta_name(&meta, "language"))
            .or_else(|| meta_name(&meta, "lang"));

        // Short title if provided by JSON-LD
        let shorttitle = json_short_title(&json_ld).or_else(|| derive_short_title_local(&title));

        // 5) Item type mapping (compatibility-minded heuristic)
        // Preserve EMT quirk when no clear conference container is present.
        let entry_ty = if booktitle.is_some() {
            "@inproceedings"
        } else if journaltitle.is_some() {
            "@article"
        } else {
            // USENIX talk pages sometimes look like articles in EMT; mirror that bias.
            "@article"
        };

        // 6) Build BibLaTeX fields
        let mut fields: Vec<(String, String)> = Vec::new();
        // Post-process title: strip unescaped braces repeatedly, then unescape \{ and \}
        let fixed_title = strip_all_unescaped_braces(&title);
        fields.push(("title".to_string(), fixed_title));
        if let Some(d) = date.clone() {
            fields.push(("date".to_string(), d));
        }
        if !authors.is_empty() {
            fields.push(("author".to_string(), authors.join(" and ")));
        }
        if let Some(lang) = language {
            // Spec requests `language`; do not use `langid` here.
            fields.push(("language".to_string(), lang));
        }
        if let Some(bt) = booktitle.clone() {
            fields.push(("booktitle".to_string(), bt));
        }
        if let Some(jt) = journaltitle.clone() {
            fields.push(("journaltitle".to_string(), jt));
        }
        if let Some(v) = volume {
            fields.push(("volume".to_string(), v));
        }
        if let Some(n) = number {
            fields.push(("number".to_string(), n));
        }
        if let Some(p) = pages {
            fields.push(("pages".to_string(), p));
        }
        if let Some(d) = doi.take() {
            fields.push(("doi".to_string(), d));
        }
        if let Some(i) = isbn {
            fields.push(("isbn".to_string(), i));
        }
        fields.push(("url".to_string(), url.as_str().to_string()));
        if let Some(s) = shorttitle {
            fields.push(("shorttitle".to_string(), s));
        }

        // 7) Construct and parse BibLaTeX
        let key = build_key("usenix", &final_url);
        let mut out = String::new();
        out.push_str(entry_ty);
        out.push('{');
        out.push_str(&key);
        out.push_str(",\n");
        for (k, v) in fields {
            out.push_str("    ");
            out.push_str(&k);
            out.push_str(" = {");
            out.push_str(&escape_braces(&v));
            out.push_str("},\n");
        }
        out.push_str("}\n");

        let bib = Bibliography::parse(&out)
            .map_err(|e| anyhow::anyhow!("failed to parse constructed BibLaTeX: {e}"))?;
        let entry = bib
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("empty bibliography from USENIX translator"))?;

        // Ensure `title` is non-empty post processing
        // Minimal validation: ensure fields exist (values are token sequences; don't introspect here).
        if entry.get("title").is_none() { return Err(anyhow::anyhow!("ValidationError: empty title for URL {}", final_url)); }
        if entry.get("url").is_none() { return Err(anyhow::anyhow!("ValidationError: missing url for URL {}", final_url)); }

        Ok(entry)
    }
}

impl IdFamily for Usenix {
    type For<'a> = Usenix;
}

// ----------------------------
// Implementation helpers (duplicated minimally from embedded.rs for isolation)
// ----------------------------

fn fetch(url: Url) -> anyhow::Result<(Url, String)> {
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build();
    let agent = ureq::Agent::new_with_config(cfg);
    let req = agent.get(url.as_str()).header(
        "User-Agent",
        "Mozilla/5.0 (compatible; bib/0.1; +https://www.usenix.org)",
    );
    let res = req
        .call()
        .with_context(|| format!("failed request for URL {}", url))?;

    // Determine effective final URL and check Content-Type for HTML
    let headers = res.headers();
    if let Some(ctv) = headers.get("content-type")
        && let Ok(cts) = ctv.to_str()
    {
        let cts = cts.to_ascii_lowercase();
        if !cts.contains("text/html") && !cts.contains("application/xhtml") && !cts.contains("html") {
            return Err(anyhow::anyhow!(
                "FetchError: non-HTML content-type for URL {}: {}",
                url, cts
            ));
        }
    }

    // ureq 3.x returns an http::Response; attempt to obtain the effective URL via Debug header if any,
    // otherwise fall back to the requested URL. (ureq follows redirects internally.)
    let effective_url = if let Some(u) = headers.get("x-final-url").and_then(|v| v.to_str().ok()) {
        Url::parse(u).unwrap_or_else(|_| url.clone())
    } else {
        // If the server sets Content-Location, prefer it as the canonical representation.
        if let Some(u) = headers.get("content-location").and_then(|v| v.to_str().ok()) {
            Url::parse(u).unwrap_or_else(|_| url.clone())
        } else {
            url.clone()
        }
    };

    let body = res.into_body().read_to_string().context("read body")?;
    Ok((effective_url, body))
}

#[derive(Debug, Clone)]
struct MetaTag {
    name: Option<String>,
    property: Option<String>,
    content: String,
}

static META_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<meta\b[^>]*>"#).unwrap());
static ATTR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)([a-zA-Z_:\-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
});
static TITLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<title[^>]*>(.*?)</title>"#).unwrap());
static SCRIPT_LD_JSON_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)<script\b[^>]*type\s*=\s*["']application/ld\+json["'][^>]*>(.*?)</script>"#)
        .unwrap()
});

fn collect_meta(html: &str) -> Vec<MetaTag> {
    META_TAG_RE
        .find_iter(html)
        .filter_map(|m| parse_meta_tag(m.as_str()))
        .collect()
}

fn parse_meta_tag(tag: &str) -> Option<MetaTag> {
    let mut name = None;
    let mut property = None;
    let mut content = None;
    for cap in ATTR_RE.captures_iter(tag) {
        let key = &cap[1];
        let val = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str().to_string());
        if let Some(val) = val {
            match key.to_ascii_lowercase().as_str() {
                "name" => name = Some(val),
                "property" => property = Some(val),
                "content" => content = Some(val),
                _ => {}
            }
        }
    }
    let content = content?;
    Some(MetaTag { name, property, content })
}

fn collect_json_ld(html: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for c in SCRIPT_LD_JSON_RE.captures_iter(html) {
        if let Some(m) = c.get(1) {
            let raw = m.as_str().trim();
            let cleaned = raw
                .replace("<!--", "")
                .replace("-->", "")
                .replace("\u{0000}", "");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned) {
                match v {
                    serde_json::Value::Array(a) => out.extend(a),
                    _ => out.push(v),
                }
            }
        }
    }
    out
}

fn collect_title(html: &str) -> Option<String> {
    TITLE_RE
        .captures(html)
        .and_then(|c| c.get(1).map(|m| normalize_ws(m.as_str())))
}

fn meta_value(metas: &[MetaTag], name: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.name.as_deref() == Some(name))
        .map(|m| m.content.trim().to_string())
}

fn meta_name(metas: &[MetaTag], name: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.name.as_deref() == Some(name))
        .map(|m| m.content.trim().to_string())
}

fn meta_property(metas: &[MetaTag], prop: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.property.as_deref() == Some(prop))
        .map(|m| m.content.trim().to_string())
}

fn json_ld_types(json_ld: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(t) = obj.get("@type")
        {
            if let Some(s) = t.as_str() {
                out.push(s.to_string());
            } else if let Some(a) = t.as_array() {
                out.extend(a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())));
            }
        }
    }
    out
}

fn json_name(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object() {
            if let Some(s) = obj.get("name").and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
            if let Some(s) = obj.get("headline").and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn json_is_part_of_name(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(o) = obj.get("isPartOf")
            && let Some(name) = o
                .as_object()
                .and_then(|oo| oo.get("name"))
                .and_then(|x| x.as_str())
        {
            return Some(name.to_string());
        }
    }
    None
}

fn json_url(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(s) = obj.get("url").and_then(|x| x.as_str())
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_short_title(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(s) = obj.get("alternativeHeadline").and_then(|x| x.as_str())
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_authors(json_ld: &[serde_json::Value]) -> Option<Vec<String>> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(a) = obj.get("author")
        {
            if let Some(s) = a.as_str() {
                return Some(split_creators(s));
            }
            if let Some(arr) = a.as_array() {
                let mut out = Vec::new();
                for it in arr {
                    if let Some(s) = it.as_str() { out.push(s.to_string()); continue; }
                    if let Some(o) = it.as_object()
                        && let Some(n) = o.get("name").and_then(|x| x.as_str())
                    {
                        out.push(n.to_string());
                    }
                }
                if !out.is_empty() { return Some(out); }
            }
        }
    }
    None
}

fn json_date_published(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(s) = obj.get("datePublished").and_then(|x| x.as_str())
        {
            return Some(s.to_string());
        }
    }
    None
}

fn extend_creators(out: &mut Vec<String>, metas: &[MetaTag], name: &str) {
    for m in metas.iter().filter(|m| m.name.as_deref() == Some(name)) {
        let s = m.content.trim();
        if !s.is_empty() && !looks_like_url_or_handle(s) {
            out.push(s.to_string());
        }
    }
}

fn extend_creators_split(out: &mut Vec<String>, metas: &[MetaTag], name: &str) {
    if let Some(v) = meta_value(metas, name) {
        for s in split_creators(&v) {
            if !s.is_empty() && !looks_like_url_or_handle(&s) {
                out.push(s);
            }
        }
    }
}

fn split_creators(s: &str) -> Vec<String> {
    let t = s.trim();
    if t.contains(';') {
        t.split(';').map(normalize_name).collect()
    } else if t.contains(" and ") {
        t.split(" and ").map(normalize_name).collect()
    } else if t.split(',').count() > 1 {
        t.split(',').map(normalize_name).collect()
    } else {
        vec![normalize_name(t)]
    }
}

fn normalize_name(s: &str) -> String {
    normalize_ws(s).trim_matches(',').trim().to_string()
}

fn looks_like_url_or_handle(s: &str) -> bool {
    s.contains('@') || s.starts_with('@') || s.starts_with("http://") || s.starts_with("https://")
}

fn dedup_in_place(v: &mut Vec<String>) {
    let mut seen = std::collections::BTreeSet::new();
    v.retain(|x| seen.insert(x.to_ascii_lowercase()));
}

fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn strip_site_suffix(title: &str, site: &str) -> String {
    let site_esc = regex::escape(site.trim());
    let re = Regex::new(&format!(r"(?i)\s*[\-–—=|:~#]\s*{}\s*$", site_esc)).unwrap();
    re.replace(title, "").trim().to_string()
}

fn build_pages(first: Option<String>, last: Option<String>) -> Option<String> {
    match (first, last) {
        (Some(f), Some(l)) => {
            let f = f.replace(['\u{2013}', '\u{2014}'], "-").trim().to_string();
            let l = l.replace(['\u{2013}', '\u{2014}'], "-").trim().to_string();
            if f.is_empty() && l.is_empty() {
                None
            } else {
                Some(format!("{}-{}", f, l))
            }
        }
        (Some(f), None) | (None, Some(f)) => {
            let f = f.replace(['\u{2013}', '\u{2014}'], "-");
            Some(f)
        }
        _ => None,
    }
}

fn clean_doi(s: String) -> Option<String> {
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\b(10\.\d{4,9}/[-._;()/:A-Z0-9]+)\b").unwrap());
    RE.captures(&s)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn normalise_date(s: &str) -> Option<String> {
    let t = s.trim();
    static ISO_FULL: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4})[-/](\d{2})[-/](\d{2})").unwrap());
    static ISO_YM: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4})[-/](\d{2})\b").unwrap());
    static ISO_Y: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4})\b").unwrap());
    if let Some(c) = ISO_FULL.captures(t) {
        return Some(format!("{}-{}-{}", &c[1], &c[2], &c[3]));
    }
    if let Some(c) = ISO_YM.captures(t) {
        return Some(format!("{}-{}", &c[1], &c[2]));
    }
    if let Some(c) = ISO_Y.captures(t) {
        return Some(c[1].to_string());
    }
    static RFC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})[T\s].*").unwrap());
    if let Some(c) = RFC_RE.captures(t) {
        return Some(c[1].to_string());
    }
    None
}

fn absolutise(base: &Url, cand: &str) -> anyhow::Result<Url> {
    if let Ok(u) = Url::parse(cand) {
        return Ok(u);
    }
    if cand.starts_with("//") {
        return Url::parse(&format!("{}:{}", base.scheme(), cand)).map_err(|e| e.into());
    }
    base.join(cand).map_err(|e| e.into())
}

fn escape_braces(s: &str) -> String {
    s.replace('{', "\\{").replace('}', "\\}")
}

fn build_key(prefix: &str, url: &Url) -> String {
    let host = url.host_str().unwrap_or("www.usenix.org");
    let path = url.path().trim_matches('/');
    let slug = if path.is_empty() { "root".to_string() } else { path.replace('/', "-") };
    format!("{}:{}:{}", prefix, host, slug)
}

// ----------------------------
// USENIX-specific post-processing: strip all unescaped braces in title
// ----------------------------

fn strip_all_unescaped_braces(s: &str) -> String {
    // Emulate JS regex: repeatedly remove pairs (?<!\\)\{([^{}]*)\}
    fn strip_once(s: &str) -> Option<String> {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i: usize = 0;
        let mut emit_from: usize = 0;
        let mut changed = false;
        while i < bytes.len() {
            if bytes[i] == b'{' && (i == 0 || bytes[i - 1] != b'\\') {
                // candidate opening brace
                let mut j = i + 1;
                let mut bad = false;
                while j < bytes.len() && bytes[j] != b'}' {
                    if bytes[j] == b'{' || bytes[j] == b'}' {
                        bad = true;
                        break;
                    }
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'}' && !bad {
                    // emit preceding segment and inner content, skip the braces
                    if emit_from < i {
                        out.push_str(&s[emit_from..i]);
                    }
                    out.push_str(&s[i + 1..j]);
                    changed = true;
                    i = j + 1;
                    emit_from = i;
                    continue;
                }
            }
            i += 1;
        }
        if changed {
            out.push_str(&s[emit_from..]);
            Some(out)
        } else {
            None
        }
    }

    let mut cur = s.to_string();
    while let Some(next) = strip_once(&cur) {
        cur = next;
    }
    cur.replace(r"\{", "{").replace(r"\}", "}")
}

fn derive_short_title_local(title: &str) -> Option<String> {
    if let Some((head, _tail)) = title.split_once(':') {
        let h = head.trim();
        if !h.is_empty() && h.len() + 3 < title.len() {
            return Some(h.to_string());
        }
    }
    None
}

// ----------------------------
// Tests (detector only, networked extraction is environment-skipped in integration tests)
// ----------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_presentation_url() {
        let url = "https://www.usenix.org/conference/pepr25/presentation/sharma";
        assert!(<Usenix as Identifier>::parse(url).is_some());
    }

    #[test]
    fn parse_rejects_non_canonical_hosts_and_paths() {
        for bad in [
            // http rejected in compatibility mode
            "http://www.usenix.org/conference/pepr25/presentation/sharma",
            // non-www rejected in compatibility mode
            "https://usenix.org/conference/pepr25/presentation/sharma",
            // different path family
            "https://www.usenix.org/event/pepr25/presentation/sharma",
            // presentation missing
            "https://www.usenix.org/conference/pepr25/program",
            // other subdomains
            "https://blog.usenix.org/conference/pepr25/presentation/sharma",
        ] {
            assert!(<Usenix as Identifier>::parse(bad).is_none(), "should reject {bad}");
        }
    }

    #[test]
    fn strip_all_unescaped_braces_behaviour() {
        assert_eq!(strip_all_unescaped_braces("{BLAKE2}"), "BLAKE2");
        assert_eq!(strip_all_unescaped_braces("{A {B} C}"), "A B C");
        assert_eq!(strip_all_unescaped_braces("\\{esc\\}"), "{esc}");
        assert_eq!(strip_all_unescaped_braces("nest {one {two}} end"), "nest one two end");
    }
}
