# CLI

`bib` has two subcommands, namely

- `fetch`, which fetches information about the reference items, and
- `pull`, which pulls files related to the reference items (PDF, HTML, etc.).

Both of them accept a list of either identifiers, or bibliography files, where the latter can be either

- BibTeX, or
- Hayagriva.

A bibliography file will be treated as a list of items, while an identifier will be treated as a singular item.
