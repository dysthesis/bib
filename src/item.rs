pub struct Item {
    item_type: ItemType,
    title: Option<String>,
    author: Vec<Author>,
    issued: Option<chrono::Utc>,
    doi: Option<String>,
    url: String,
    container_title: Option<String>,
    language: Option<String>,
    abstract_: Option<String>,
    provenance: Vec<Provenance>,
}

pub enum ItemType {
    WebPage,
}

pub struct Author {
    pub family: Option<String>,
    pub given: Option<String>,
    pub literal: Option<String>,
}

/// Where each information from `item` is extracted from
pub struct Provenance {
    pub field: String,
    pub source: String,
}
