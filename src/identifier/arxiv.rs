use anyhow::Context;
use biblatex::{Bibliography, Entry};
use once_cell::sync::Lazy;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use regex::Regex;

use crate::{identifier::Identifier, resolver::IdFamily};

/// An arXiv identifier or URL, normalised to its canonical ID and optional version.
pub struct Arxiv<'a> {
    /// Canonical arXiv ID without version (e.g., "1810.04805" or "astro-ph/0603274").
    canonical_id: &'a str,
    /// Optional explicit version number when present in the input (e.g., Some("2")).
    version: Option<&'a str>,
    /// Whether the ID is legacy-style (contains a slash) or new-style.
    legacy: bool,
}

impl<'a> Identifier<'a> for Arxiv<'a> {
    fn parse(identifier: &'a str) -> Option<Box<Self>> {
        let mut s = identifier.trim();

        // Strip URL wrappers and handle prefixes.
        if let Some(rest) = s
            .strip_prefix("arXiv:")
            .or_else(|| s.strip_prefix("arxiv:"))
        {
            s = rest.trim_start();
        }

        // Try arXiv URLs (abs/pdf), including legacy host alias.
        // Examples:
        //  - https://arxiv.org/abs/1810.04805v2
        //  - https://arxiv.org/pdf/1810.04805v2.pdf
        //  - https://xxx.lanl.gov/abs/astro-ph/0603274v1
        if let Some((host, path)) = s
            .strip_prefix("http://")
            .or_else(|| s.strip_prefix("https://"))
            .and_then(|rest| rest.split_once('/'))
        {
            let host = host.to_ascii_lowercase();
            if host.ends_with("arxiv.org")
                || host.ends_with("xxx.lanl.gov")
                || host.ends_with("export.arxiv.org")
            {
                let mut comps = path.split('?').next().unwrap_or(path);
                // Trim trailing fragment and .pdf suffix if present.
                if let Some((base, _frag)) = comps.split_once('#') {
                    comps = base;
                }
                if let Some(rest) = comps.strip_prefix("abs/") {
                    s = rest;
                } else if let Some(p) = comps.strip_prefix("pdf/") {
                    s = p.strip_suffix(".pdf").unwrap_or(p);
                } else if comps.starts_with("find/")
                    || comps.starts_with("list/")
                    || comps.starts_with("search/")
                {
                    // Multi-item page: out of scope for core Identifier; signal unparseable here.
                    return None;
                } else {
                    // Unknown arXiv path
                    return None;
                }
            }
        }

        // Now handle bare IDs (new-style or legacy-style), optionally with version suffix.
        static NEWSTYLE_RE: Lazy<Regex> = Lazy::new(|| {
            // new-style: YYYY.NNNNN(vN)? or older YYMM.NNNN(vN)?; accept 4 digits dot 4–5 digits
            Regex::new(r"^(?P<core>\d{4}\.[0-9]{4,5})(?:v(?P<v>\d+))?$").unwrap()
        });
        static LEGACY_RE: Lazy<Regex> = Lazy::new(|| {
            // legacy: archive(/subject)?/NNNNNNN with optional vN, allow dots in archive (e.g., math.GT)
            Regex::new(r"^(?P<core>[A-Za-z-]+(?:\.[A-Za-z-]+)?/[0-9]{7})(?:v(?P<v>\d+))?$").unwrap()
        });

        let s = s.trim_matches('/');

        if let Some(c) = NEWSTYLE_RE.captures(s) {
            let core = c.name("core").unwrap().as_str();
            let ver = c.name("v").map(|m| m.as_str());
            return Some(Box::new(Arxiv {
                canonical_id: core,
                version: ver,
                legacy: false,
            }));
        }
        if let Some(c) = LEGACY_RE.captures(s) {
            let core = c.name("core").unwrap().as_str();
            let ver = c.name("v").map(|m| m.as_str());
            return Some(Box::new(Arxiv {
                canonical_id: core,
                version: ver,
                legacy: true,
            }));
        }

        None
    }

    fn resolve(&self) -> anyhow::Result<Entry> {
        let atom = fetch_atom(self.canonical_id)?;
        let meta = parse_atom_entry(&atom, self.canonical_id)?;

        // Construct BibLaTeX string and parse into an Entry to avoid depending on biblatex internals.
        let bib = build_biblatex(&meta, self.canonical_id, self.version, self.legacy);
        let bib = Bibliography::parse(&bib)
            .map_err(|e| anyhow::anyhow!("failed to parse constructed BibLaTeX: {e}"))?;
        let entry =
            bib.iter().next().cloned().ok_or_else(|| {
                anyhow::anyhow!("empty bibliography from constructed arXiv record")
            })?;
        Ok(entry)
    }
}

impl IdFamily for Arxiv<'_> {
    type For<'a> = Arxiv<'a>;
}

/// Normalised arXiv Atom metadata we care about.
struct ArxivMeta {
    title: String,
    summary: String,
    updated: Option<String>,
    authors: Vec<String>,
    published_doi: Option<String>,
    primary_class: Option<String>,
    categories: Vec<String>,
    comments: Vec<String>,
}

fn fetch_atom(id: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse("https://export.arxiv.org/api/query")?;
    url.query_pairs_mut()
        .append_pair("id_list", id)
        .append_pair("max_results", "1");
    let cfg = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build();
    let agent = ureq::Agent::new_with_config(cfg);
    let body: String = agent
        .get(url.as_str())
        .header(
            "User-Agent",
            "Mozilla/5.0 (compatible; bib/0.1; +https://arxiv.org)",
        )
        .call()
        .with_context(|| format!("failed Atom request for arXiv id {id}"))?
        .into_body()
        .read_to_string()
        .context("failed to read Atom response body")?;
    Ok(body)
}

fn parse_atom_entry(xml: &str, id: &str) -> anyhow::Result<ArxivMeta> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut in_entry = false;
    let mut in_author = false;
    let mut cur_text = String::new();

    let mut title = String::new();
    let mut summary = String::new();
    let mut updated = None;
    let mut authors = Vec::new();
    let mut published_doi: Option<String> = None;
    let mut primary_class: Option<String> = None;
    let mut categories: Vec<String> = Vec::new();
    let mut comments: Vec<String> = Vec::new();

    fn is_local(name: &[u8], target: &str) -> bool {
        // Compare local name ignoring namespace prefixes.
        if let Some(pos) = name.iter().rposition(|&b| b == b':') {
            &name[pos + 1..] == target.as_bytes()
        } else {
            name == target.as_bytes()
        }
    }

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                if is_local(e.name().as_ref(), "entry") {
                    in_entry = true;
                } else if in_entry && is_local(e.name().as_ref(), "author") {
                    in_author = true;
                } else if in_entry && is_local(e.name().as_ref(), "primary_category") {
                    if let Some(val) = get_attr_value(&e, b"term") {
                        primary_class = Some(val);
                    }
                } else if in_entry && is_local(e.name().as_ref(), "category") {
                    if let Some(val) = get_attr_value(&e, b"term") {
                        categories.push(val);
                    }
                } else if in_entry && is_local(e.name().as_ref(), "link") {
                    let rel = get_attr_value(&e, b"rel");
                    if matches!(rel.as_deref(), Some("related"))
                        && let Some(href) = get_attr_value(&e, b"href")
                        && let Some(doi) = extract_doi_from_url(&href)
                    {
                        published_doi.get_or_insert(doi);
                    }
                }
                cur_text.clear();
            }
            Ok(Event::End(e)) => {
                if is_local(e.name().as_ref(), "entry") {
                    break; // finished first entry
                } else if is_local(e.name().as_ref(), "author") {
                    in_author = false;
                } else if in_entry && is_local(e.name().as_ref(), "title") {
                    title = normalize_ws(&cur_text);
                } else if in_entry && is_local(e.name().as_ref(), "summary") {
                    summary = cur_text.trim().to_string();
                } else if in_entry && is_local(e.name().as_ref(), "updated") {
                    let t = cur_text.trim();
                    if !t.is_empty() {
                        updated = Some(t.to_string());
                    }
                } else if in_entry && in_author && is_local(e.name().as_ref(), "name") {
                    let n = cur_text.trim();
                    if !n.is_empty() {
                        authors.push(n.to_string());
                    }
                } else if in_entry && is_local(e.name().as_ref(), "doi") {
                    // <arxiv:doi>
                    let d = cur_text.trim();
                    if !d.is_empty() {
                        published_doi.get_or_insert(d.to_string());
                    }
                } else if in_entry && is_local(e.name().as_ref(), "comment") {
                    let c = cur_text.trim();
                    if !c.is_empty() {
                        comments.push(c.to_string());
                    }
                }
                cur_text.clear();
            }
            Ok(Event::Text(t)) => {
                cur_text.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::CData(t)) => {
                cur_text.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::Empty(e)) => {
                if in_entry && is_local(e.name().as_ref(), "primary_category") {
                    if let Some(val) = get_attr_value(&e, b"term") {
                        primary_class = Some(val);
                    }
                } else if in_entry && is_local(e.name().as_ref(), "category") {
                    if let Some(val) = get_attr_value(&e, b"term") {
                        categories.push(val);
                    }
                } else if in_entry && is_local(e.name().as_ref(), "link") {
                    let rel = get_attr_value(&e, b"rel");
                    if matches!(rel.as_deref(), Some("related"))
                        && let Some(href) = get_attr_value(&e, b"href")
                        && let Some(doi) = extract_doi_from_url(&href)
                    {
                        published_doi.get_or_insert(doi);
                    }
                }
            }
            Err(e) => return Err(anyhow::anyhow!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    if title.is_empty() && summary.is_empty() && authors.is_empty() {
        return Err(anyhow::anyhow!("no Atom entry found for arXiv id {id}"));
    }

    Ok(ArxivMeta {
        title,
        summary,
        updated,
        authors,
        published_doi,
        primary_class,
        categories,
        comments,
    })
}

fn get_attr_value(e: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == key)
        .map(|a| String::from_utf8_lossy(a.value.as_ref()).to_string())
}

fn extract_doi_from_url(url: &str) -> Option<String> {
    // Accept https://doi.org/10.x/yyy or http(s)://dx.doi.org/10.x/yyy
    static DOI_IN_URL: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)https?://(?:dx\.)?doi\.org/(?P<doi>10\.\d{4,9}/[-._;()/:A-Z0-9]+)")
            .unwrap()
    });
    DOI_IN_URL
        .captures(url)
        .and_then(|c| c.name("doi").map(|m| m.as_str().to_string()))
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

fn build_biblatex(meta: &ArxivMeta, id: &str, version: Option<&str>, legacy: bool) -> String {
    let key = format!("arXiv:{}", id);
    let url = format!("https://arxiv.org/abs/{}", id);
    // PDF URL derivable from ID; omitted in BibLaTeX fields.
    let doi = meta
        .published_doi
        .clone()
        .unwrap_or_else(|| format!("10.48550/arXiv.{}", id));

    // Map categories to human-readable keywords.
    let mut tags: Vec<String> = Vec::new();
    for term in &meta.categories {
        if let Some(label) = map_category(term, meta.primary_class.as_deref())
            && !tags.contains(&label)
        {
            tags.push(label);
        }
    }
    // Ensure primary class-derived tag appears if categories list missed it.
    if tags.is_empty()
        && let Some(pc) = &meta.primary_class
        && let Some(label) = map_category(pc, meta.primary_class.as_deref())
    {
        tags.push(label);
    }

    let mut fields = Vec::new();
    fields.push(format!("title = {{{}}}", meta.title));
    if !meta.summary.trim().is_empty() {
        fields.push(format!("abstract = {{{}}}", meta.summary));
    }
    if let Some(d) = &meta.updated {
        fields.push(format!("date = {{{}}}", d));
    }
    if !meta.authors.is_empty() {
        let authors = meta.authors.join(" and ");
        fields.push(format!("author = {{{}}}", authors));
    }
    fields.push(format!("doi = {{{}}}", doi));
    fields.push(format!("url = {{{}}}", url));
    fields.push("eprinttype = {arXiv}".to_string());
    fields.push(format!("eprint = {{{}}}", id));
    if !legacy
        && let Some(class) = meta.primary_class.as_deref().and_then(primary_class_of)
    {
        fields.push(format!("eprintclass = {{{}}}", class));
    }
    if let Some(v) = version {
        fields.push(format!("eprintversion = {{{}}}", v));
    }
    // Venue-like fields for arXiv-only (these do not overwrite DOI-derived entries since we don't merge):
    fields.push("publisher = {arXiv}".to_string());
    fields.push(format!("number = {{{}}}", key));
    if !tags.is_empty() {
        fields.push(format!("keywords = {{{}}}", tags.join(", ")));
    }
    if !meta.comments.is_empty() {
        // Concatenate comments as a single note with Comment: prefix for each.
        let mut note = String::new();
        for (i, c) in meta.comments.iter().enumerate() {
            if i > 0 {
                note.push_str("; ");
            }
            note.push_str("Comment: ");
            note.push_str(c);
        }
        fields.push(format!("note = {{{}}}", note));
    }
    // Include a PDF hint via howpublished or addenda? BibLaTeX has no attachment; omit but keep URL.
    // Build the entry.
    let mut out = String::new();
    out.push_str(&format!("@online{{{},\n", key));
    for f in fields {
        out.push_str("    ");
        out.push_str(&f);
        out.push_str(",\n");
    }
    out.push_str("}\n");
    out
}

fn primary_class_of(term: &str) -> Option<&'static str> {
    // For dot-coded categories, take the archive before the dot.
    let head = term.split('.').next().unwrap_or(term);
    Some(match head {
        // Major archives (comprehensive)
        "cs" => "cs",
        "econ" => "econ",
        "eess" => "eess",
        "math" => "math",
        "astro-ph" => "astro-ph",
        "cond-mat" => "cond-mat",
        "gr-qc" => "gr-qc",
        "hep-ex" => "hep-ex",
        "hep-lat" => "hep-lat",
        "hep-ph" => "hep-ph",
        "hep-th" => "hep-th",
        "math-ph" => "math-ph",
        "nlin" => "nlin",
        "nucl-ex" => "nucl-ex",
        "nucl-th" => "nucl-th",
        "physics" => "physics",
        "quant-ph" => "quant-ph",
        "q-bio" => "q-bio",
        "q-fin" => "q-fin",
        "stat" => "stat",
        _ => return None,
    })
}

fn map_category(term: &str, primary: Option<&str>) -> Option<String> {
    // Direct full-term mappings take precedence (e.g., math-ph -> Mathematical Physics).
    if let Some(lbl) = FULL_TERM_LABELS.get(term) {
        return Some(lbl.to_string());
    }

    // Try to split into archive and subcategory.
    if let Some((arch, _)) = term.split_once('.')
        && let (Some(arch_name), Some(sub_name)) =
            (ARCHIVE_NAMES.get(arch), SUBCATEGORY_NAMES.get(term))
    {
        return Some(format!("{} - {}", arch_name, sub_name));
    }

    // If no mapping, but primary archive known and there is a display name for it, return just the archive name.
    if let Some(p) = primary.and_then(|p| ARCHIVE_NAMES.get(p)) {
        return Some((*p).to_string());
    }
    None
}

static ARCHIVE_NAMES: Lazy<std::collections::HashMap<&'static str, &'static str>> = Lazy::new(|| {
    use std::collections::HashMap;
    let mut m = HashMap::new();
    // Core archives
    m.insert("cs", "Computer Science");
    m.insert("econ", "Economics");
    m.insert("eess", "Electrical Engineering and Systems Science");
    m.insert("math", "Mathematics");
    m.insert("astro-ph", "Astrophysics");
    m.insert("cond-mat", "Condensed Matter");
    m.insert("gr-qc", "General Relativity and Quantum Cosmology");
    m.insert("hep-ex", "High Energy Physics - Experiment");
    m.insert("hep-lat", "High Energy Physics - Lattice");
    m.insert("hep-ph", "High Energy Physics - Phenomenology");
    m.insert("hep-th", "High Energy Physics - Theory");
    m.insert("math-ph", "Mathematical Physics");
    m.insert("nlin", "Nonlinear Sciences");
    m.insert("nucl-ex", "Nuclear Experiment");
    m.insert("nucl-th", "Nuclear Theory");
    m.insert("physics", "Physics");
    m.insert("quant-ph", "Quantum Physics");
    m.insert("q-bio", "Quantitative Biology");
    m.insert("q-fin", "Quantitative Finance");
    m.insert("stat", "Statistics");
    m
});

static SUBCATEGORY_NAMES: Lazy<std::collections::HashMap<&'static str, &'static str>> =
    Lazy::new(|| {
        use std::collections::HashMap;
        let mut m = HashMap::new();
        // Computer Science
        m.insert("cs.AI", "Artificial Intelligence");
        m.insert("cs.AR", "Hardware Architecture");
        m.insert("cs.CC", "Computational Complexity");
        m.insert("cs.CE", "Computational Engineering, Finance, and Science");
        m.insert("cs.CG", "Computational Geometry");
        m.insert("cs.CL", "Computation and Language");
        m.insert("cs.CR", "Cryptography and Security");
        m.insert("cs.CV", "Computer Vision and Pattern Recognition");
        m.insert("cs.CY", "Computers and Society");
        m.insert("cs.DB", "Databases");
        m.insert("cs.DC", "Distributed, Parallel, and Cluster Computing");
        m.insert("cs.DL", "Digital Libraries");
        m.insert("cs.DM", "Discrete Mathematics");
        m.insert("cs.DS", "Data Structures and Algorithms");
        m.insert("cs.ET", "Emerging Technologies");
        m.insert("cs.FL", "Formal Languages and Automata Theory");
        m.insert("cs.GL", "General Literature");
        m.insert("cs.GR", "Graphics");
        m.insert("cs.GT", "Computer Science and Game Theory");
        m.insert("cs.HC", "Human-Computer Interaction");
        m.insert("cs.IR", "Information Retrieval");
        m.insert("cs.IT", "Information Theory");
        m.insert("cs.LG", "Machine Learning");
        m.insert("cs.LO", "Logic in Computer Science");
        m.insert("cs.MA", "Multiagent Systems");
        m.insert("cs.MM", "Multimedia");
        m.insert("cs.MS", "Mathematical Software");
        m.insert("cs.NA", "Numerical Analysis"); // alias of math.NA
        m.insert("cs.NE", "Neural and Evolutionary Computing");
        m.insert("cs.NI", "Networking and Internet Architecture");
        m.insert("cs.OH", "Other Computer Science");
        m.insert("cs.OS", "Operating Systems");
        m.insert("cs.PF", "Performance");
        m.insert("cs.PL", "Programming Languages");
        m.insert("cs.RO", "Robotics");
        m.insert("cs.SC", "Symbolic Computation");
        m.insert("cs.SD", "Sound");
        m.insert("cs.SE", "Software Engineering");
        m.insert("cs.SI", "Social and Information Networks");
        m.insert("cs.SY", "Systems and Control"); // alias of eess.SY

        // Economics
        m.insert("econ.EM", "Econometrics");
        m.insert("econ.GN", "General Economics");
        m.insert("econ.TH", "Theoretical Economics");

        // Electrical Engineering and Systems Science
        m.insert("eess.AS", "Audio and Speech Processing");
        m.insert("eess.IV", "Image and Video Processing");
        m.insert("eess.SP", "Signal Processing");
        m.insert("eess.SY", "Systems and Control");

        // Mathematics
        m.insert("math.AC", "Commutative Algebra");
        m.insert("math.AG", "Algebraic Geometry");
        m.insert("math.AP", "Analysis of PDEs");
        m.insert("math.AT", "Algebraic Topology");
        m.insert("math.CA", "Classical Analysis and ODEs");
        m.insert("math.CO", "Combinatorics");
        m.insert("math.CT", "Category Theory");
        m.insert("math.CV", "Complex Variables");
        m.insert("math.DG", "Differential Geometry");
        m.insert("math.DS", "Dynamical Systems");
        m.insert("math.FA", "Functional Analysis");
        m.insert("math.GM", "General Mathematics");
        m.insert("math.GN", "General Topology");
        m.insert("math.GR", "Group Theory");
        m.insert("math.GT", "Geometric Topology");
        m.insert("math.HO", "History and Overview");
        m.insert("math.IT", "Information Theory"); // alias of cs.IT
        m.insert("math.KT", "K-Theory and Homology");
        m.insert("math.LO", "Logic");
        m.insert("math.MG", "Metric Geometry");
        m.insert("math.NA", "Numerical Analysis");
        m.insert("math.NT", "Number Theory");
        m.insert("math.OA", "Operator Algebras");
        m.insert("math.OC", "Optimization and Control");
        m.insert("math.PR", "Probability");
        m.insert("math.QA", "Quantum Algebra");
        m.insert("math.RA", "Rings and Algebras");
        m.insert("math.RT", "Representation Theory");
        m.insert("math.SG", "Symplectic Geometry");
        m.insert("math.SP", "Spectral Theory");
        m.insert("math.ST", "Statistics Theory");

        // Astrophysics (astro-ph)
        m.insert("astro-ph.CO", "Cosmology and Nongalactic Astrophysics");
        m.insert("astro-ph.EP", "Earth and Planetary Astrophysics");
        m.insert("astro-ph.GA", "Astrophysics of Galaxies");
        m.insert("astro-ph.HE", "High Energy Astrophysical Phenomena");
        m.insert("astro-ph.IM", "Instrumentation and Methods for Astrophysics");
        m.insert("astro-ph.SR", "Solar and Stellar Astrophysics");

        // Condensed Matter (cond-mat)
        m.insert("cond-mat.dis-nn", "Disordered Systems and Neural Networks");
        m.insert("cond-mat.mes-hall", "Mesoscale and Nanoscale Physics");
        m.insert("cond-mat.mtrl-sci", "Materials Science");
        m.insert("cond-mat.other", "Other Condensed Matter");
        m.insert("cond-mat.quant-gas", "Quantum Gases");
        m.insert("cond-mat.soft", "Soft Condensed Matter");
        m.insert("cond-mat.stat-mech", "Statistical Mechanics");
        m.insert("cond-mat.str-el", "Strongly Correlated Electrons");
        m.insert("cond-mat.supr-con", "Superconductivity");

        // Nonlinear Sciences (nlin)
        m.insert("nlin.AO", "Adaptation and Self-Organizing Systems");
        m.insert("nlin.CD", "Chaotic Dynamics");
        m.insert("nlin.CG", "Cellular Automata and Lattice Gases");
        m.insert("nlin.PS", "Pattern Formation and Solitons");
        m.insert("nlin.SI", "Exactly Solvable and Integrable Systems");

        // Physics (physics.*)
        m.insert("physics.acc-ph", "Accelerator Physics");
        m.insert("physics.ao-ph", "Atmospheric and Oceanic Physics");
        m.insert("physics.app-ph", "Applied Physics");
        m.insert("physics.atm-clus", "Atomic and Molecular Clusters");
        m.insert("physics.atom-ph", "Atomic Physics");
        m.insert("physics.bio-ph", "Biological Physics");
        m.insert("physics.chem-ph", "Chemical Physics");
        m.insert("physics.class-ph", "Classical Physics");
        m.insert("physics.comp-ph", "Computational Physics");
        m.insert("physics.data-an", "Data Analysis, Statistics and Probability");
        m.insert("physics.ed-ph", "Physics Education");
        m.insert("physics.flu-dyn", "Fluid Dynamics");
        m.insert("physics.gen-ph", "General Physics");
        m.insert("physics.geo-ph", "Geophysics");
        m.insert("physics.hist-ph", "History and Philosophy of Physics");
        m.insert("physics.ins-det", "Instrumentation and Detectors");
        m.insert("physics.med-ph", "Medical Physics");
        m.insert("physics.optics", "Optics");
        m.insert("physics.plasm-ph", "Plasma Physics");
        m.insert("physics.pop-ph", "Popular Physics");
        m.insert("physics.soc-ph", "Physics and Society");
        m.insert("physics.space-ph", "Space Physics");

        // Quantitative Biology
        m.insert("q-bio.BM", "Biomolecules");
        m.insert("q-bio.CB", "Cell Behavior");
        m.insert("q-bio.GN", "Genomics");
        m.insert("q-bio.MN", "Molecular Networks");
        m.insert("q-bio.NC", "Neurons and Cognition");
        m.insert("q-bio.OT", "Other Quantitative Biology");
        m.insert("q-bio.PE", "Populations and Evolution");
        m.insert("q-bio.QM", "Quantitative Methods");
        m.insert("q-bio.SC", "Subcellular Processes");
        m.insert("q-bio.TO", "Tissues and Organs");

        // Quantitative Finance
        m.insert("q-fin.CP", "Computational Finance");
        m.insert("q-fin.EC", "Economics"); // alias of econ.GN
        m.insert("q-fin.GN", "General Finance");
        m.insert("q-fin.MF", "Mathematical Finance");
        m.insert("q-fin.PM", "Portfolio Management");
        m.insert("q-fin.PR", "Pricing of Securities");
        m.insert("q-fin.RM", "Risk Management");
        m.insert("q-fin.ST", "Statistical Finance");
        m.insert("q-fin.TR", "Trading and Market Microstructure");

        // Statistics
        m.insert("stat.AP", "Applications");
        m.insert("stat.CO", "Computation");
        m.insert("stat.ME", "Methodology");
        m.insert("stat.ML", "Machine Learning");
        m.insert("stat.OT", "Other Statistics");
        m.insert("stat.TH", "Statistics Theory"); // alias of math.ST
        m
    });

static FULL_TERM_LABELS: Lazy<std::collections::HashMap<&'static str, &'static str>> =
    Lazy::new(|| {
        use std::collections::HashMap;
        let mut m = HashMap::new();
        // Standalone archives / aliases without subcategories
        m.insert("math-ph", "Mathematical Physics");
        m.insert("gr-qc", "General Relativity and Quantum Cosmology");
        m.insert("hep-ex", "High Energy Physics - Experiment");
        m.insert("hep-lat", "High Energy Physics - Lattice");
        m.insert("hep-ph", "High Energy Physics - Phenomenology");
        m.insert("hep-th", "High Energy Physics - Theory");
        m.insert("nucl-ex", "Nuclear Experiment");
        m.insert("nucl-th", "Nuclear Theory");
        m.insert("quant-ph", "Quantum Physics");
        // Aliases expressed as non-dot codes
        m.insert("math.MP", "Mathematical Physics"); // alias to math-ph
        m
    });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_new_style_bare_and_version() {
        let a = <Arxiv<'_> as Identifier<'_>>::parse("1810.04805").unwrap();
        assert_eq!(a.canonical_id, "1810.04805");
        assert!(a.version.is_none());

        let b = <Arxiv<'_> as Identifier<'_>>::parse("1810.04805v2").unwrap();
        assert_eq!(b.canonical_id, "1810.04805");
        assert_eq!(b.version, Some("2"));
    }

    #[test]
    fn parse_legacy_and_prefix_and_url() {
        let a = <Arxiv<'_> as Identifier<'_>>::parse("astro-ph/0603274v1").unwrap();
        assert_eq!(a.canonical_id, "astro-ph/0603274");
        assert_eq!(a.version, Some("1"));
        assert!(a.legacy);

        let b = <Arxiv<'_> as Identifier<'_>>::parse("arXiv:1810.04805v3").unwrap();
        assert_eq!(b.canonical_id, "1810.04805");
        assert_eq!(b.version, Some("3"));

        let c =
            <Arxiv<'_> as Identifier<'_>>::parse("https://arxiv.org/pdf/1810.04805v1.pdf").unwrap();
        assert_eq!(c.canonical_id, "1810.04805");
        assert_eq!(c.version, Some("1"));
    }

    #[test]
    fn build_keywords_mapping() {
        // cs.CL maps to "Computer Science - Computation and Language"
        let lbl = map_category("cs.CL", Some("cs")).unwrap();
        assert_eq!(lbl, "Computer Science - Computation and Language");

        // math-ph has a direct label
        let lbl2 = map_category("math-ph", None).unwrap();
        assert_eq!(lbl2, "Mathematical Physics");
    }
}
