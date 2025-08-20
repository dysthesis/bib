use anyhow::Context;
use biblatex::{Bibliography, Entry};
use once_cell::sync::Lazy;
use regex::Regex;
use url::Url;

use crate::{identifier::Identifier, resolver::IdFamily};

/// A generic, last-resort translator for HTTP(S) webpages using embedded metadata and
/// conservative heuristics.
pub struct Embedded {
    url: Url,
}

impl<'a> Identifier<'a> for Embedded {
    fn parse(identifier: &'a str) -> Option<Box<Self>> {
        let url = Url::parse(identifier).ok()?;
        match url.scheme() {
            "http" | "https" => {}
            _ => return None,
        }
        // Decline known utility/decoy endpoints
        let u = url.as_str();
        if u.contains("jetpack.wordpress.com/jetpack-comment/") {
            return None;
        }
        Some(Box::new(Embedded { url }))
    }

    fn resolve(&self) -> anyhow::Result<Entry> {
        let (final_url, html) = fetch(self.url.clone())?;
        let base_url = final_url; // may include redirects; used for absolutising

        // Collect signals
        let meta = collect_meta(&html);
        let links = collect_links(&html);
        let title_tag = collect_title(&html);
        let html_lang = collect_html_lang(&html);
        let json_ld = collect_json_ld(&html);

        // Canonical URL
        let canonical = links
            .iter()
            .find(|l| l.rel.eq_ignore_ascii_case("canonical"))
            .and_then(|l| absolutise(&base_url, &l.href).ok())
            .unwrap_or_else(|| base_url.clone());

        // HighWire presence
        let has_highwire = meta.iter().any(|m| {
            m.name
                .as_deref()
                .map(|n| n.starts_with("citation_"))
                .unwrap_or(false)
        });

        // Item type inference (conservative)
        let item_ty = if has_highwire {
            if meta_value(&meta, "citation_conference_title").is_some()
                || meta_value(&meta, "citation_conference").is_some()
            {
                ItemTy::InProceedings
            } else if meta_value(&meta, "citation_dissertation_institution").is_some() {
                ItemTy::Thesis
            } else if meta_value(&meta, "citation_technical_report_institution").is_some() {
                ItemTy::Report
            } else if meta_value(&meta, "citation_journal_title").is_some() {
                ItemTy::Article
            } else if meta_value(&meta, "citation_inbook_title").is_some() {
                ItemTy::InCollection
            } else {
                ItemTy::Online
            }
        } else {
            // Default fallback to an online/webpage-like entry
            ItemTy::Online
        };

        // Field extraction with precedence
        let site_name = meta_property(&meta, "og:site_name");
        let mut title = meta_value(&meta, "citation_title")
            .or_else(|| json_headline(&json_ld))
            .or_else(|| meta_property(&meta, "og:title"))
            .or_else(|| title_tag.clone())
            .unwrap_or_else(|| base_url.as_str().to_string());
        title = normalize_ws(&title);
        if let Some(site) = site_name {
            title = strip_site_suffix(&title, &site);
        }
/* RESOLVED: HEAD version */

        let mut authors = Vec::new();
        // HighWire authors
        extend_creators(&mut authors, &meta, "citation_author");
        extend_creators_split(&mut authors, &meta, "citation_authors");
        // Schema.org
        if authors.is_empty() && let Some(list) = json_authors(&json_ld) {
            authors.extend(list);
        }
        // OpenGraph article:author (ignore URLs)
        if authors.is_empty() {
            authors.extend(
                meta.iter()
                    .filter(|m| m.property.as_deref() == Some("article:author"))
                    .filter_map(|m| {
                        let v = m.content.trim();
                        if Url::parse(v).is_ok() || v.is_empty() { None } else { Some(v.to_string()) }
                    }),
            );
        }
        // W3C author meta
        if authors.is_empty() && let Some(a) = meta_name(&meta, "author") {
            authors.extend(split_creators(&a));
        }
        dedup_in_place(&mut authors);

        // Editors (HighWire)
        let mut editors = Vec::new();
        extend_creators(&mut editors, &meta, "citation_editor");
        extend_creators_split(&mut editors, &meta, "citation_editors");
        dedup_in_place(&mut editors);

        // Date precedence
        let date = meta_value(&meta, "citation_publication_date")
            .or_else(|| meta_value(&meta, "citation_cover_date"))
            .or_else(|| meta_value(&meta, "citation_date"))
            .or_else(|| {
                let online = meta_value(&meta, "citation_online_date");
                let year = meta_value(&meta, "citation_year");
                match (online, year) {
                    (Some(o), Some(y)) => Some(pick_earlier_year(&o, &y)),
                    (Some(o), None) => Some(o),
                    (None, Some(y)) => Some(y),
                    _ => None,
                }
            })
            .or_else(|| json_date_published(&json_ld))
            .or_else(|| meta_property(&meta, "article:published_time"))
            .or_else(|| collect_time_datetime(&html))
            .and_then(|d| normalise_date(&d));

        // Container
        let journal = meta_value(&meta, "citation_journal_title");
        let inbook = meta_value(&meta, "citation_inbook_title");
        let book = meta_value(&meta, "citation_book_title");

        // Volume/issue/pages
        let volume = meta_value(&meta, "citation_volume");
        let issue = meta_value(&meta, "citation_issue");
        let pages = build_pages(
            meta_value(&meta, "citation_firstpage"),
            meta_value(&meta, "citation_lastpage"),
        );

        // Identifiers
        let mut doi = meta_value(&meta, "citation_doi").and_then(clean_doi);
        let issn = meta_value_any(&meta, &["citation_issn", "citation_ISSN"]);
        let eissn = meta_value(&meta, "citation_eIssn");
        // Prefer print ISSN when both present
        let issn_clean = issn.or(eissn);

        // URL precedence
        let url = meta_value(&meta, "citation_public_url")
            .or_else(|| meta_value(&meta, "citation_abstract_html_url"))
            .or_else(|| meta_value(&meta, "citation_fulltext_html_url"))
            .or_else(|| meta_property(&meta, "og:url"))
            .and_then(|u| absolutise(&base_url, &u).ok())
            .unwrap_or_else(|| canonical.clone());

        // Language precedence
        let language = meta_value(&meta, "citation_language")
            .or_else(|| meta_name(&meta, "language"))
            .or_else(|| meta_name(&meta, "lang"))
            .or_else(|| meta_http_equiv(&meta, "content-language"))
            .or(html_lang);

        // Abstract
        let abstract_note = meta_value(&meta, "citation_abstract")
            .or_else(|| json_description(&json_ld))
            .or_else(|| meta_name(&meta, "description"));

        // Tags
        let mut keywords = split_tags(
            meta_value(&meta, "citation_keywords")
                .or_else(|| json_keywords(&json_ld))
                .or_else(|| meta_name(&meta, "keywords"))
                .unwrap_or_default(),
        );
        dedup_in_place(&mut keywords);

        // Attachments policy: we do not support BibLaTeX attachments; if a PDF URL exists, we do not add it
        // to BibLaTeX. This is intentionally omitted.

        // Access date: YYYY-MM-DD
        let urldate = chrono::Utc::now().date_naive().to_string();

        // Build BibLaTeX
        let mut fields: Vec<(String, String)> = Vec::new();
        fields.push(("title".to_string(), title));
        if let Some(d) = date.clone() {
            fields.push(("date".to_string(), d));
        }
        if !authors.is_empty() {
            fields.push(("author".to_string(), authors.join(" and ")));
        }
        if !editors.is_empty() {
            fields.push(("editor".to_string(), editors.join(" and ")));
        }
        if let Some(lang) = language {
            fields.push(("langid".to_string(), lang));
        }
        if let Some(abs) = abstract_note {
            fields.push(("abstract".to_string(), normalize_ws(&abs)));
        }
        if let Some(j) = journal.clone() {
            fields.push(("journaltitle".to_string(), j));
        }
        if let Some(ib) = inbook.clone() {
            fields.push(("booktitle".to_string(), ib));
        } else if journal.is_none() && let Some(b) = book.clone() {
            fields.push(("booktitle".to_string(), b));
        }
        if let Some(v) = volume {
            fields.push(("volume".to_string(), v));
        }
        if let Some(i) = issue {
            fields.push(("number".to_string(), i));
        }
        if let Some(p) = pages {
            fields.push(("pages".to_string(), p));
        }
        if let Some(d) = doi.take() {
            fields.push(("doi".to_string(), d));
        }
        if let Some(i) = issn_clean {
            fields.push(("issn".to_string(), i));
        }
        // ISBN is not handled in this minimal implementation.
        fields.push(("url".to_string(), url.as_str().to_string()));
        fields.push(("urldate".to_string(), urldate));
        if !keywords.is_empty() {
            fields.push(("keywords".to_string(), keywords.join(", ")));
        }

        // Publisher/institution/university
        if let Some(p) = meta_value(&meta, "citation_publisher") {
            fields.push(("publisher".to_string(), p));
        }
        if let Some(u) = meta_value(&meta, "citation_dissertation_institution") {
            fields.push(("institution".to_string(), u));
        }
        if let Some(inst) = meta_value(&meta, "citation_technical_report_institution") {
            fields.push(("institution".to_string(), inst));
        }
        if let Some(n) = meta_value(&meta, "citation_technical_report_number") {
            fields.push(("number".to_string(), n));
        }

        // Conference name
        if let Some(conf) = meta_value(&meta, "citation_conference_title")
            .or_else(|| meta_value(&meta, "citation_conference"))
        {
            fields.push(("eventtitle".to_string(), conf));
        }

        // Build entry type and key
        let (entry_ty, key) = match item_ty {
            ItemTy::Article => ("@article", build_key("article", &canonical)),
            ItemTy::InProceedings => ("@inproceedings", build_key("conf", &canonical)),
            ItemTy::Thesis => ("@thesis", build_key("thesis", &canonical)),
            ItemTy::Report => ("@report", build_key("report", &canonical)),
            ItemTy::InCollection => ("@incollection", build_key("incollection", &canonical)),
            ItemTy::Online => ("@online", build_key("web", &canonical)),
        };

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
            .ok_or_else(|| anyhow::anyhow!("empty bibliography from embedded translator"))?;
        Ok(entry)
    }
}

impl IdFamily for Embedded {
    type For<'a> = Embedded;
}

// ----------------------------
// Helpers and collectors
// ----------------------------

fn fetch(url: Url) -> anyhow::Result<(Url, String)> {
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build();
    let agent = ureq::Agent::new_with_config(cfg);
    let req = agent.get(url.as_str()).header(
        "User-Agent",
        "Mozilla/5.0 (compatible; bib/0.1; +https://example.org)",
    );
    let res = req
        .call()
        .with_context(|| format!("failed request for URL {}", url))?;
    let body = res.into_body().read_to_string().context("read body")?;
    // Honour <base href> when present for absolutising relative URLs.
    let base = if let Some(href) = collect_base_href(&body) {
        absolutise(&url, &href).unwrap_or(url)
    } else {
        url
    };
    Ok((base, body))
}

#[derive(Debug, Clone)]
struct MetaTag {
    name: Option<String>,
    property: Option<String>,
    http_equiv: Option<String>,
    content: String,
}

#[derive(Debug, Clone)]
struct LinkTag {
    rel: String,
    href: String,
}

static META_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<meta\b[^>]*>"#).unwrap());
static ATTR_RE: Lazy<Regex> = Lazy::new(|| {
    // Attribute pairs: key="value" or key='value' (no backreferences in Rust regex)
    Regex::new(r#"(?i)([a-zA-Z_:\-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
});
static LINK_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<link\b[^>]*>"#).unwrap());
static TITLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<title[^>]*>(.*?)</title>"#).unwrap());
static HTML_LANG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<html\b[^>]*>"#).unwrap());
static TIME_DT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<time\b[^>]*?datetime\s*=\s*(?:"([^"]*)"|'([^']*)')[^>]*>"#).unwrap());
static SCRIPT_LD_JSON_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?is)<script\b[^>]*type\s*=\s*["']application/ld\+json["'][^>]*>(.*?)</script>"#).unwrap());
static BASE_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<base\b[^>]*>"#).unwrap());

fn collect_meta(html: &str) -> Vec<MetaTag> {
    META_TAG_RE
        .find_iter(html)
        .filter_map(|m| parse_meta_tag(m.as_str()))
        .collect()
}

fn parse_meta_tag(tag: &str) -> Option<MetaTag> {
    let mut name = None;
    let mut property = None;
    let mut http_equiv = None;
    let mut content = None;
    for cap in ATTR_RE.captures_iter(tag) {
        let key = &cap[1];
        let val = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str().to_string());
        if let Some(val) = val {
            match key.to_ascii_lowercase().as_str() {
                "name" => name = Some(val),
                "property" => property = Some(val),
                "http-equiv" => http_equiv = Some(val),
                "content" => content = Some(val),
                _ => {}
            }
        }
    }
    let content = content?;
    Some(MetaTag {
        name,
        property,
        http_equiv,
        content,
    })
}

fn collect_links(html: &str) -> Vec<LinkTag> {
    LINK_TAG_RE
        .find_iter(html)
        .filter_map(|m| parse_link_tag(m.as_str()))
        .collect()
}

fn parse_link_tag(tag: &str) -> Option<LinkTag> {
    let mut rel = None;
    let mut href = None;
    for cap in ATTR_RE.captures_iter(tag) {
        let key = &cap[1];
        let val = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str().to_string());
        if let Some(val) = val {
            match key.to_ascii_lowercase().as_str() {
                "rel" => rel = Some(val),
                "href" => href = Some(val),
                _ => {}
            }
        }
    }
    Some(LinkTag {
        rel: rel?,
        href: href?,
    })
}

fn collect_title(html: &str) -> Option<String> {
    TITLE_RE
        .captures(html)
        .and_then(|c| c.get(1).map(|m| normalize_ws(m.as_str())))
}

fn collect_html_lang(html: &str) -> Option<String> {
    HTML_LANG_RE.find(html).and_then(|m| {
        let tag = m.as_str();
        for cap in ATTR_RE.captures_iter(tag) {
            let key = &cap[1];
            let val = cap
                .get(2)
                .or_else(|| cap.get(3))
                .map(|m| m.as_str().to_string());
            if key.eq_ignore_ascii_case("lang") && let Some(v) = val {
                return Some(v);
            }
        }
        None
    })
}

fn collect_time_datetime(html: &str) -> Option<String> {
    TIME_DT_RE
        .captures(html)
        .and_then(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().to_string())
}

fn collect_base_href(html: &str) -> Option<String> {
    if let Some(m) = BASE_TAG_RE.find(html) {
        let tag = m.as_str();
        for cap in ATTR_RE.captures_iter(tag) {
            let key = &cap[1];
            let val = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str().to_string());
            if key.eq_ignore_ascii_case("href") { return val; }
        }
        }
    None
}

fn collect_json_ld(html: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for c in SCRIPT_LD_JSON_RE.captures_iter(html) {
        if let Some(m) = c.get(1) {
            let raw = m.as_str().trim();
            // Relax common issues: strip HTML comments and trailing commas crudely
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

fn meta_value(metas: &[MetaTag], name: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.name.as_deref() == Some(name))
        .map(|m| m.content.trim().to_string())
}

fn meta_value_any(metas: &[MetaTag], names: &[&str]) -> Option<String> {
    for n in names {
        if let Some(v) = meta_value(metas, n) {
            return Some(v);
        }
    }
    None
}

fn meta_name(metas: &[MetaTag], name: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.name.as_deref().map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false))
        .map(|m| m.content.trim().to_string())
}

fn meta_http_equiv(metas: &[MetaTag], key: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.http_equiv.as_deref().map(|n| n.eq_ignore_ascii_case(key)).unwrap_or(false))
        .map(|m| m.content.trim().to_string())
}

fn meta_property(metas: &[MetaTag], prop: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.property.as_deref() == Some(prop))
        .map(|m| m.content.trim().to_string())
}

// Blog heuristics intentionally omitted in minimal fallback.

// (Video detection intentionally omitted in this minimal implementation.)

fn json_headline(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("headline").or_else(|| obj.get("name"))
            && let Some(s) = h.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_date_published(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("datePublished")
            && let Some(s) = h.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_description(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("description")
            && let Some(s) = h.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_keywords(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("keywords")
        {
            if let Some(s) = h.as_str() { return Some(s.to_string()); }
            if let Some(a) = h.as_array() {
                return Some(a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "));
            }
        }
    }
    None
}

fn json_authors(json_ld: &[serde_json::Value]) -> Option<Vec<String>> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(a) = obj.get("author")
        {
            if let Some(s) = a.as_str() { return Some(split_creators(s)); }
            if let Some(arr) = a.as_array() {
                let mut out = Vec::new();
                for it in arr {
                    if let Some(s) = it.as_str() { out.push(s.to_string()); continue; }
                    if let Some(o) = it.as_object() && let Some(n) = o.get("name").and_then(|x| x.as_str()) { out.push(n.to_string()); }
                }
                if !out.is_empty() { return Some(out); }
            }
        }
    }
    None
}

fn extend_creators(out: &mut Vec<String>, metas: &[MetaTag], name: &str) {
    for m in metas.iter().filter(|m| m.name.as_deref() == Some(name)) {
        let s = m.content.trim();
        if !s.is_empty() && !looks_like_url_or_handle(s) { out.push(s.to_string()); }
    }
}

fn extend_creators_split(out: &mut Vec<String>, metas: &[MetaTag], name: &str) {
    if let Some(v) = meta_value(metas, name) {
        for s in split_creators(&v) {
            if !s.is_empty() && !looks_like_url_or_handle(&s) { out.push(s); }
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
            if !prev_space { out.push(' '); prev_space = true; }
        } else { out.push(ch); prev_space = false; }
    }
    out.trim().to_string()
}

fn split_tags(s: String) -> Vec<String> {
    let t = s.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let parts: Vec<&str> = if t.contains(';') {
        t.split(';').collect()
    } else if t.contains(',') {
        t.split(',').collect()
    } else {
        vec![t]
    };
    for p in parts {
        let owned = normalize_ws(p);
        let w = owned.trim_matches(|c: char| c == ',' || c == ';').trim();
        if !w.is_empty() {
            out.push(w.to_string());
        }
    }
    out
}

fn strip_site_suffix(title: &str, site: &str) -> String {
    // Strip common separators when site name appears at end
    let site_esc = regex::escape(site.trim());
    let re = Regex::new(&format!(r"(?i)\s*[\-–—=|:~#]\s*{}\s*$", site_esc)).unwrap();
    re.replace(title, "").trim().to_string()
}

fn pick_earlier_year(online: &str, year: &str) -> String {
    let oy = extract_year(online).unwrap_or_default();
    let cy = extract_year(year).unwrap_or_default();
    if oy > cy && cy > 0 { year.to_string() } else { online.to_string() }
/* RESOLVED: parent of 6314c19 (Embedded metadata resolver) */
/* RESOLVED: begin duplicate from 6314c19 (commented out)
        let shorttitle = derive_short_title(&title);

        let mut authors = Vec::new();
        // HighWire authors
        extend_creators(&mut authors, &meta, "citation_author");
        extend_creators_split(&mut authors, &meta, "citation_authors");
        // Schema.org
        if authors.is_empty() && let Some(list) = json_authors(&json_ld) {
            authors.extend(list);
        }
        // OpenGraph article:author (ignore URLs)
        if authors.is_empty() {
            authors.extend(
                meta.iter()
                    .filter(|m| m.property.as_deref() == Some("article:author"))
                    .filter_map(|m| {
                        let v = m.content.trim();
                        if Url::parse(v).is_ok() || v.is_empty() { None } else { Some(v.to_string()) }
                    }),
            );
        }
        // W3C author meta
        if authors.is_empty() && let Some(a) = meta_name(&meta, "author") {
            authors.extend(split_creators(&a));
        }
        // Low-quality byline heuristic: anchor rel="author"
        if authors.is_empty() && let Some(a) = extract_rel_author(&html) {
            let a = invert_simple_name(&a);
            authors.push(a);
        }
        dedup_in_place(&mut authors);

        // Editors (HighWire)
        let mut editors = Vec::new();
        extend_creators(&mut editors, &meta, "citation_editor");
        extend_creators_split(&mut editors, &meta, "citation_editors");
        dedup_in_place(&mut editors);

        // Date precedence
        let date = meta_value(&meta, "citation_publication_date")
            .or_else(|| meta_value(&meta, "citation_cover_date"))
            .or_else(|| meta_value(&meta, "citation_date"))
            .or_else(|| {
                let online = meta_value(&meta, "citation_online_date");
                let year = meta_value(&meta, "citation_year");
                match (online, year) {
                    (Some(o), Some(y)) => Some(pick_earlier_year(&o, &y)),
                    (Some(o), None) => Some(o),
                    (None, Some(y)) => Some(y),
                    _ => None,
                }
            })
            .or_else(|| json_date_published(&json_ld))
            .or_else(|| meta_property(&meta, "article:published_time"))
            .or_else(|| collect_time_datetime(&html))
            .and_then(|d| normalise_date(&d));

        // Container
        let journal = meta_value(&meta, "citation_journal_title");
        let inbook = meta_value(&meta, "citation_inbook_title");
        let book = meta_value(&meta, "citation_book_title");

        // Volume/issue/pages
        let volume = meta_value(&meta, "citation_volume");
        let issue = meta_value(&meta, "citation_issue");
        let pages = build_pages(
            meta_value(&meta, "citation_firstpage"),
            meta_value(&meta, "citation_lastpage"),
        );

        // Identifiers
        let mut doi = meta_value(&meta, "citation_doi").and_then(clean_doi);
        let issn = meta_value_any(&meta, &["citation_issn", "citation_ISSN"]);
        let eissn = meta_value(&meta, "citation_eIssn");
        // Prefer print ISSN when both present
        let issn_clean = issn.or(eissn);

        // URL precedence
        let url = meta_value(&meta, "citation_public_url")
            .or_else(|| meta_value(&meta, "citation_abstract_html_url"))
            .or_else(|| meta_value(&meta, "citation_fulltext_html_url"))
            .or_else(|| meta_property(&meta, "og:url"))
            .and_then(|u| absolutise(&base_url, &u).ok())
            .unwrap_or_else(|| canonical.clone());

        // Language precedence
        let language = meta_value(&meta, "citation_language")
            .or_else(|| meta_name(&meta, "language"))
            .or_else(|| meta_name(&meta, "lang"))
            .or_else(|| meta_http_equiv(&meta, "content-language"))
            .or(html_lang);

        // Abstract
        let abstract_note = meta_value(&meta, "citation_abstract")
            .or_else(|| json_description(&json_ld))
            .or_else(|| meta_name(&meta, "description"));

        // Tags
        let mut keywords = split_tags(
            meta_value(&meta, "citation_keywords")
                .or_else(|| json_keywords(&json_ld))
                .or_else(|| meta_name(&meta, "keywords"))
                .unwrap_or_default(),
        );
        dedup_in_place(&mut keywords);

        // Attachments policy: we do not support BibLaTeX attachments; if a PDF URL exists, we do not add it
        // to BibLaTeX. This is intentionally omitted.

        // Access date: YYYY-MM-DD
        let urldate = chrono::Utc::now().date_naive().to_string();

        // Build BibLaTeX
        let mut fields: Vec<(String, String)> = Vec::new();
        fields.push(("title".to_string(), title));
        if let Some(d) = date.clone() {
            fields.push(("date".to_string(), d));
        }
        if !authors.is_empty() {
            fields.push(("author".to_string(), authors.join(" and ")));
        }
        if !editors.is_empty() {
            fields.push(("editor".to_string(), editors.join(" and ")));
        }
        if let Some(lang) = language {
            fields.push(("langid".to_string(), lang));
        }
        if let Some(abs) = abstract_note {
            fields.push(("abstract".to_string(), normalize_ws(&abs)));
        }
        if let Some(j) = journal.clone() {
            fields.push(("journaltitle".to_string(), j));
        }
        if let Some(ib) = inbook.clone() {
            fields.push(("booktitle".to_string(), ib));
        } else if journal.is_none() && let Some(b) = book.clone() {
            fields.push(("booktitle".to_string(), b));
        }
        if let Some(v) = volume {
            fields.push(("volume".to_string(), v));
        }
        if let Some(i) = issue {
            fields.push(("number".to_string(), i));
        }
        if let Some(p) = pages {
            fields.push(("pages".to_string(), p));
        }
        if let Some(d) = doi.take() {
            fields.push(("doi".to_string(), d));
        }
        if let Some(i) = issn_clean {
            fields.push(("issn".to_string(), i));
        }
        // ISBN is not handled in this minimal implementation.
        fields.push(("url".to_string(), url.as_str().to_string()));
        fields.push(("urldate".to_string(), urldate));
        if !keywords.is_empty() {
            fields.push(("keywords".to_string(), keywords.join(", ")));
        }
        if let Some(s) = shorttitle.clone() {
            // In BibLaTeX this is 'shorttitle'
            fields.push(("shorttitle".to_string(), s));
        }
        if let Some(site) = meta_property(&meta, "og:site_name") {
            // Represent the website title; BibLaTeX often uses 'organization' for @online
            fields.push(("organization".to_string(), site));
        }

        // Publisher/institution/university
        if let Some(p) = meta_value(&meta, "citation_publisher") {
            fields.push(("publisher".to_string(), p));
        }
        if let Some(u) = meta_value(&meta, "citation_dissertation_institution") {
            fields.push(("institution".to_string(), u));
        }
        if let Some(inst) = meta_value(&meta, "citation_technical_report_institution") {
            fields.push(("institution".to_string(), inst));
        }
        if let Some(n) = meta_value(&meta, "citation_technical_report_number") {
            fields.push(("number".to_string(), n));
        }

        // Conference name
        if let Some(conf) = meta_value(&meta, "citation_conference_title")
            .or_else(|| meta_value(&meta, "citation_conference"))
        {
            fields.push(("eventtitle".to_string(), conf));
        }

        // Build entry type and key
        let (entry_ty, key) = match item_ty {
            ItemTy::Article => ("@article", build_key("article", &canonical)),
            ItemTy::InProceedings => ("@inproceedings", build_key("conf", &canonical)),
            ItemTy::Thesis => ("@thesis", build_key("thesis", &canonical)),
            ItemTy::Report => ("@report", build_key("report", &canonical)),
            ItemTy::InCollection => ("@incollection", build_key("incollection", &canonical)),
            ItemTy::Online => ("@online", build_key("web", &canonical)),
        };

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
            .ok_or_else(|| anyhow::anyhow!("empty bibliography from embedded translator"))?;
        Ok(entry)
    }
}

impl IdFamily for Embedded {
    type For<'a> = Embedded;
}

// ----------------------------
// Helpers and collectors
// ----------------------------

fn fetch(url: Url) -> anyhow::Result<(Url, String)> {
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build();
    let agent = ureq::Agent::new_with_config(cfg);
    let req = agent.get(url.as_str()).header(
        "User-Agent",
        "Mozilla/5.0 (compatible; bib/0.1; +https://example.org)",
    );
    let res = req
        .call()
        .with_context(|| format!("failed request for URL {}", url))?;
    let body = res.into_body().read_to_string().context("read body")?;
    // Honour <base href> when present for absolutising relative URLs.
    let base = if let Some(href) = collect_base_href(&body) {
        absolutise(&url, &href).unwrap_or(url)
    } else {
        url
    };
    Ok((base, body))
}

#[derive(Debug, Clone)]
struct MetaTag {
    name: Option<String>,
    property: Option<String>,
    http_equiv: Option<String>,
    content: String,
}

#[derive(Debug, Clone)]
struct LinkTag {
    rel: String,
    href: String,
}

static META_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<meta\b[^>]*>"#).unwrap());
static ATTR_RE: Lazy<Regex> = Lazy::new(|| {
    // Attribute pairs: key="value" or key='value' (no backreferences in Rust regex)
    Regex::new(r#"(?i)([a-zA-Z_:\-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
});
static LINK_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<link\b[^>]*>"#).unwrap());
static TITLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<title[^>]*>(.*?)</title>"#).unwrap());
static HTML_LANG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<html\b[^>]*>"#).unwrap());
static TIME_DT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<time\b[^>]*?datetime\s*=\s*(?:"([^"]*)"|'([^']*)')[^>]*>"#).unwrap());
static SCRIPT_LD_JSON_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?is)<script\b[^>]*type\s*=\s*["']application/ld\+json["'][^>]*>(.*?)</script>"#).unwrap());
static BASE_TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<base\b[^>]*>"#).unwrap());

fn collect_meta(html: &str) -> Vec<MetaTag> {
    META_TAG_RE
        .find_iter(html)
        .filter_map(|m| parse_meta_tag(m.as_str()))
        .collect()
}

fn parse_meta_tag(tag: &str) -> Option<MetaTag> {
    let mut name = None;
    let mut property = None;
    let mut http_equiv = None;
    let mut content = None;
    for cap in ATTR_RE.captures_iter(tag) {
        let key = &cap[1];
        let val = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str().to_string());
        if let Some(val) = val {
            match key.to_ascii_lowercase().as_str() {
                "name" => name = Some(val),
                "property" => property = Some(val),
                "http-equiv" => http_equiv = Some(val),
                "content" => content = Some(val),
                _ => {}
            }
        }
    }
    let content = content?;
    Some(MetaTag {
        name,
        property,
        http_equiv,
        content,
    })
}

fn collect_links(html: &str) -> Vec<LinkTag> {
    LINK_TAG_RE
        .find_iter(html)
        .filter_map(|m| parse_link_tag(m.as_str()))
        .collect()
}

fn parse_link_tag(tag: &str) -> Option<LinkTag> {
    let mut rel = None;
    let mut href = None;
    for cap in ATTR_RE.captures_iter(tag) {
        let key = &cap[1];
        let val = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str().to_string());
        if let Some(val) = val {
            match key.to_ascii_lowercase().as_str() {
                "rel" => rel = Some(val),
                "href" => href = Some(val),
                _ => {}
            }
        }
    }
    Some(LinkTag {
        rel: rel?,
        href: href?,
    })
}

fn collect_title(html: &str) -> Option<String> {
    TITLE_RE
        .captures(html)
        .and_then(|c| c.get(1).map(|m| normalize_ws(m.as_str())))
}

fn collect_html_lang(html: &str) -> Option<String> {
    HTML_LANG_RE.find(html).and_then(|m| {
        let tag = m.as_str();
        for cap in ATTR_RE.captures_iter(tag) {
            let key = &cap[1];
            let val = cap
                .get(2)
                .or_else(|| cap.get(3))
                .map(|m| m.as_str().to_string());
            if key.eq_ignore_ascii_case("lang") && let Some(v) = val {
                return Some(v);
            }
        }
        None
    })
}

fn collect_time_datetime(html: &str) -> Option<String> {
    TIME_DT_RE
        .captures(html)
        .and_then(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().to_string())
}

fn collect_base_href(html: &str) -> Option<String> {
    if let Some(m) = BASE_TAG_RE.find(html) {
        let tag = m.as_str();
        for cap in ATTR_RE.captures_iter(tag) {
            let key = &cap[1];
            let val = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str().to_string());
            if key.eq_ignore_ascii_case("href") { return val; }
        }
        }
    None
}

fn collect_json_ld(html: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for c in SCRIPT_LD_JSON_RE.captures_iter(html) {
        if let Some(m) = c.get(1) {
            let raw = m.as_str().trim();
            // Relax common issues: strip HTML comments and trailing commas crudely
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

fn meta_value(metas: &[MetaTag], name: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.name.as_deref() == Some(name))
        .map(|m| m.content.trim().to_string())
}

fn meta_value_any(metas: &[MetaTag], names: &[&str]) -> Option<String> {
    for n in names {
        if let Some(v) = meta_value(metas, n) {
            return Some(v);
        }
    }
    None
}

fn meta_name(metas: &[MetaTag], name: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.name.as_deref().map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false))
        .map(|m| m.content.trim().to_string())
}

fn meta_http_equiv(metas: &[MetaTag], key: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.http_equiv.as_deref().map(|n| n.eq_ignore_ascii_case(key)).unwrap_or(false))
        .map(|m| m.content.trim().to_string())
}

fn meta_property(metas: &[MetaTag], prop: &str) -> Option<String> {
    metas
        .iter()
        .find(|m| m.property.as_deref() == Some(prop))
        .map(|m| m.content.trim().to_string())
}

// Blog heuristics intentionally omitted in minimal fallback.

// (Video detection intentionally omitted in this minimal implementation.)

fn json_headline(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("headline").or_else(|| obj.get("name"))
            && let Some(s) = h.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_date_published(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("datePublished")
            && let Some(s) = h.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_description(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("description")
            && let Some(s) = h.as_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn json_keywords(json_ld: &[serde_json::Value]) -> Option<String> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(h) = obj.get("keywords")
        {
            if let Some(s) = h.as_str() { return Some(s.to_string()); }
            if let Some(a) = h.as_array() {
                return Some(a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "));
            }
        }
    }
    None
}

fn json_authors(json_ld: &[serde_json::Value]) -> Option<Vec<String>> {
    for v in json_ld {
        if let Some(obj) = v.as_object()
            && let Some(a) = obj.get("author")
        {
            if let Some(s) = a.as_str() { return Some(split_creators(s)); }
            if let Some(arr) = a.as_array() {
                let mut out = Vec::new();
                for it in arr {
                    if let Some(s) = it.as_str() { out.push(s.to_string()); continue; }
                    if let Some(o) = it.as_object() && let Some(n) = o.get("name").and_then(|x| x.as_str()) { out.push(n.to_string()); }
                }
                if !out.is_empty() { return Some(out); }
            }
        }
    }
    None
}

fn extend_creators(out: &mut Vec<String>, metas: &[MetaTag], name: &str) {
    for m in metas.iter().filter(|m| m.name.as_deref() == Some(name)) {
        let s = m.content.trim();
        if !s.is_empty() && !looks_like_url_or_handle(s) { out.push(s.to_string()); }
    }
}

fn extend_creators_split(out: &mut Vec<String>, metas: &[MetaTag], name: &str) {
    if let Some(v) = meta_value(metas, name) {
        for s in split_creators(&v) {
            if !s.is_empty() && !looks_like_url_or_handle(&s) { out.push(s); }
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
            if !prev_space { out.push(' '); prev_space = true; }
        } else { out.push(ch); prev_space = false; }
    }
    out.trim().to_string()
}

fn split_tags(s: String) -> Vec<String> {
    let t = s.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let parts: Vec<&str> = if t.contains(';') {
        t.split(';').collect()
    } else if t.contains(',') {
        t.split(',').collect()
    } else {
        vec![t]
    };
    for p in parts {
        let owned = normalize_ws(p);
        let w = owned.trim_matches(|c: char| c == ',' || c == ';').trim();
        if !w.is_empty() {
            out.push(w.to_string());
        }
    }
    out
}

fn strip_site_suffix(title: &str, site: &str) -> String {
    // Strip common separators when site name appears at end
    let site_esc = regex::escape(site.trim());
    let re = Regex::new(&format!(r"(?i)\s*[\-–—=|:~#]\s*{}\s*$", site_esc)).unwrap();
    re.replace(title, "").trim().to_string()
}

fn pick_earlier_year(online: &str, year: &str) -> String {
    let oy = extract_year(online).unwrap_or_default();
    let cy = extract_year(year).unwrap_or_default();
    if oy > cy && cy > 0 { year.to_string() } else { online.to_string() }
}

fn derive_short_title(title: &str) -> Option<String> {
    // Split on the first colon and trim; only return if meaningfully shorter.
    if let Some((head, _tail)) = title.split_once(':') {
        let h = head.trim();
        if !h.is_empty() && h.len() + 3 < title.len() {
            return Some(h.to_string());
        }
    }
    None
}

fn extract_rel_author(html: &str) -> Option<String> {
    // Find <a ... rel="... author ...">inner</a>
    static REL_AUTHOR_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"(?is)<a\b[^>]*\brel\s*=\s*(?:"[^"]*\bauthor\b[^"]*"|'[^']*\bauthor\b[^']*')[^>]*>(.*?)</a>"#).unwrap()
    });
    static TAG_STRIP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<[^>]+>").unwrap());
    if let Some(c) = REL_AUTHOR_RE.captures(html)
        && let Some(m) = c.get(1)
    {
        let text = TAG_STRIP_RE.replace_all(m.as_str(), "");
        let s = normalize_ws(&text);
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn invert_simple_name(name: &str) -> String {
    // Very conservative: if no comma and 2 tokens, flip to "Last, First".
    if !name.contains(',') {
        let parts: Vec<&str> = name.split_whitespace().collect();
        if parts.len() == 2 {
            return format!("{}, {}", parts[1], parts[0]);
        }
    }
    name.to_string()
*/
/* RESOLVED: end duplicate from 6314c19 */
}

fn extract_year(s: &str) -> Option<i32> {
    static YRE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\b(\d{4})\b").unwrap());
    YRE.captures(s).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok())
}

fn normalise_date(s: &str) -> Option<String> {
    let t = s.trim();
    // Try full ISO first
    static ISO_FULL: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4})[-/](\d{2})[-/](\d{2})").unwrap());
    static ISO_YM: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4})[-/](\d{2})\b").unwrap());
    static ISO_Y: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4})\b").unwrap());
    if let Some(c) = ISO_FULL.captures(t) {
        return Some(format!("{}-{}-{}", &c[1], &c[2], &c[3]));
    }
    if let Some(c) = ISO_YM.captures(t) { return Some(format!("{}-{}", &c[1], &c[2])); }
    if let Some(c) = ISO_Y.captures(t) { return Some(c[1].to_string()); }
    // RFC3339-like
    static RFC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})[T\s].*").unwrap());
    if let Some(c) = RFC_RE.captures(t) { return Some(c[1].to_string()); }
    None
}

fn build_pages(first: Option<String>, last: Option<String>) -> Option<String> {
    match (first, last) {
        (Some(f), Some(l)) => {
            let f = f.replace(['\u{2013}', '\u{2014}'], "-").trim().to_string();
            let l = l.replace(['\u{2013}', '\u{2014}'], "-").trim().to_string();
            if f.is_empty() && l.is_empty() { None } else { Some(format!("{}-{}", f, l)) }
        }
        (Some(f), None) | (None, Some(f)) => {
            let f = f.replace(['\u{2013}', '\u{2014}'], "-");
            Some(f)
        }
        _ => None,
    }
}

fn clean_doi(s: String) -> Option<String> {
    // Strip wrappers and ensure bare 10./...
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\b(10\.\d{4,9}/[-._;()/:A-Z0-9]+)\b").unwrap());
    RE.captures(&s).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

fn absolutise(base: &Url, cand: &str) -> anyhow::Result<Url> {
    if let Ok(u) = Url::parse(cand) { return Ok(u); }
    if cand.starts_with("//") {
        let mut u = base.clone();
        u.set_path("");
        return Url::parse(&format!("{}:{}", base.scheme(), cand)).map_err(|e| e.into());
    }
    base.join(cand).map_err(|e| e.into())
}

fn escape_braces(s: &str) -> String { s.replace('{', "\\{").replace('}', "\\}") }

fn build_key(prefix: &str, url: &Url) -> String {
    let host = url.host_str().unwrap_or("site");
    let path = url.path().trim_matches('/');
    let slug = if path.is_empty() { "root".to_string() } else { path.replace('/', "-") };
    format!("{}:{}:{}", prefix, host, slug)
}

#[derive(Debug, Clone, Copy)]
enum ItemTy { Article, InProceedings, Thesis, Report, InCollection, Online }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_site_suffix_basic() {
        let t = "An Interesting Post — My Blog";
        assert_eq!(strip_site_suffix(t, "My Blog"), "An Interesting Post");
    }

    #[test]
    fn normalise_date_variants() {
        assert_eq!(normalise_date("2020-01-02"), Some("2020-01-02".to_string()));
        assert_eq!(normalise_date("2020/01/02"), Some("2020-01-02".to_string()));
        assert_eq!(normalise_date("2020-01"), Some("2020-01".to_string()));
        assert_eq!(normalise_date("2020"), Some("2020".to_string()));
        assert_eq!(normalise_date("2020-01-02T10:00:00Z"), Some("2020-01-02".to_string()));
    }
}
