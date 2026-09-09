//! OpenAlex client — grounded scholarly research for Kannaka's curiosity loop.
//!
//! OpenAlex (https://openalex.org) is a free, keyless catalog of ~250M scholarly
//! works. We use it to ground Kannaka's research/curiosity arc in real
//! literature: a query returns ranked works that can be ingested into the HRM as
//! Semantic memories, so external knowledge participates in wave-resonance recall
//! and dream consolidation alongside the constellation's own memories.
//!
//! No API key is required. Supplying a `mailto` (env `KANNAKA_OPENALEX_MAILTO`)
//! opts into the faster "polite pool" — we never hardcode an address.

use std::time::Duration;

const API: &str = "https://api.openalex.org/works";

/// A single scholarly work, distilled from the OpenAlex `works` schema.
#[derive(Debug, Clone)]
pub struct Work {
    pub id: String,
    pub doi: Option<String>,
    pub title: String,
    pub year: Option<i64>,
    pub cited_by_count: i64,
    pub authors: Vec<String>,
    /// (concept name, score) — OpenAlex's topical classification.
    pub concepts: Vec<(String, f32)>,
    pub abstract_text: Option<String>,
    pub source: Option<String>,
}

impl Work {
    /// A compact, ingestion-ready rendering: title, abstract, and provenance.
    /// This becomes the `content` of an HRM memory when `--ingest` is used.
    pub fn to_memory_content(&self) -> String {
        let authors = if self.authors.is_empty() {
            String::new()
        } else {
            let shown: Vec<&str> = self.authors.iter().take(4).map(String::as_str).collect();
            let etal = if self.authors.len() > 4 { " et al." } else { "" };
            format!(" — {}{etal}", shown.join(", "))
        };
        let year = self.year.map(|y| format!(" ({y})")).unwrap_or_default();
        let src = self.source.as_deref().map(|s| format!(" [{s}]")).unwrap_or_default();
        let concepts = if self.concepts.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = self.concepts.iter().take(6).map(|(n, _)| n.as_str()).collect();
            format!("\nConcepts: {}", names.join(", "))
        };
        let abs = self.abstract_text.as_deref().unwrap_or("(no abstract)");
        format!("research: {}{year}{authors}{src}\n{abs}\nOpenAlex: {} cited_by={}{concepts}",
            self.title, self.id, self.cited_by_count)
    }

    /// Importance in [0,1] for HRM ingestion, from citation count (log-scaled,
    /// so a 10k-citation landmark doesn't dwarf everything). Floored at 0.5 so
    /// ingested research starts at least as strong as a default `remember`.
    pub fn ingest_importance(&self) -> f64 {
        let c = (self.cited_by_count.max(0) as f64 + 1.0).ln();
        // ln(1)=0 → 0.5 ; ln(~22k)≈10 → ~1.0
        (0.5 + 0.05 * c).clamp(0.5, 1.0)
    }
}

/// Search options.
pub struct SearchOpts {
    pub limit: usize,
    pub since_year: Option<i64>,
    pub min_citations: Option<i64>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self { limit: 10, since_year: None, min_citations: None }
    }
}

/// Search OpenAlex works for `query`. Returns ranked works (relevance order).
pub fn search_works(query: &str, opts: &SearchOpts) -> Result<Vec<Work>, String> {
    let per_page = opts.limit.clamp(1, 50);
    let mut url = format!("{API}?search={}&per-page={per_page}", percent_encode(query));

    // OpenAlex filter syntax: comma-joined `field:op value` clauses.
    let mut filters: Vec<String> = Vec::new();
    if let Some(y) = opts.since_year {
        filters.push(format!("publication_year:>{}", y - 1));
    }
    if let Some(c) = opts.min_citations {
        filters.push(format!("cited_by_count:>{}", c - 1));
    }
    if !filters.is_empty() {
        url.push_str("&filter=");
        url.push_str(&percent_encode(&filters.join(",")));
    }
    if let Ok(mailto) = std::env::var("KANNAKA_OPENALEX_MAILTO") {
        if !mailto.trim().is_empty() {
            url.push_str("&mailto=");
            url.push_str(&percent_encode(mailto.trim()));
        }
    }

    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
        .get(&url)
        .set("User-Agent", "kannaka-memory (research; +https://github.com/kannaka-labs/kannaka-memory)")
        .call()
        .map_err(|e| format!("OpenAlex request failed: {e}"))?;

    let json: serde_json::Value = resp.into_json()
        .map_err(|e| format!("OpenAlex parse failed: {e}"))?;

    let results = json.get("results").and_then(|v| v.as_array())
        .ok_or_else(|| "OpenAlex response missing `results`".to_string())?;

    Ok(results.iter().map(parse_work).collect())
}

fn parse_work(w: &serde_json::Value) -> Work {
    let s = |k: &str| w.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let authors = w.get("authorships").and_then(|v| v.as_array())
        .map(|a| a.iter()
            .filter_map(|au| au.get("author").and_then(|x| x.get("display_name")).and_then(|x| x.as_str()))
            .map(str::to_string)
            .collect())
        .unwrap_or_default();
    let concepts = w.get("concepts").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|c| {
            let name = c.get("display_name").and_then(|x| x.as_str())?;
            let score = c.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            Some((name.to_string(), score))
        }).collect())
        .unwrap_or_default();
    let source = w.get("primary_location")
        .and_then(|l| l.get("source"))
        .and_then(|s| s.get("display_name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Work {
        id: s("id").unwrap_or_default(),
        doi: s("doi"),
        title: s("display_name").or_else(|| s("title")).unwrap_or_else(|| "(untitled)".into()),
        year: w.get("publication_year").and_then(|v| v.as_i64()),
        cited_by_count: w.get("cited_by_count").and_then(|v| v.as_i64()).unwrap_or(0),
        authors,
        concepts,
        abstract_text: w.get("abstract_inverted_index").and_then(reconstruct_abstract),
        source,
    }
}

/// Reconstruct plain-text abstract from OpenAlex's inverted index
/// (`{word: [positions...]}`) by placing each word at each of its positions.
fn reconstruct_abstract(inv: &serde_json::Value) -> Option<String> {
    let map = inv.as_object()?;
    if map.is_empty() {
        return None;
    }
    let mut positioned: Vec<(u64, &str)> = Vec::new();
    for (word, positions) in map {
        if let Some(arr) = positions.as_array() {
            for p in arr {
                if let Some(pos) = p.as_u64() {
                    positioned.push((pos, word.as_str()));
                }
            }
        }
    }
    if positioned.is_empty() {
        return None;
    }
    positioned.sort_by_key(|(p, _)| *p);
    let text: String = positioned.iter().map(|(_, w)| *w).collect::<Vec<_>>().join(" ");
    Some(text)
}

/// Minimal percent-encoding for query-string values. Encodes everything outside
/// the RFC 3986 unreserved set, so spaces, `:`, `,`, `>` etc. travel safely.
fn percent_encode(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            // write! straight into the buffer — fmt::Write on String is
            // infallible, and this avoids a heap-allocated temporary per
            // encoded byte.
            _ => write!(out, "%{b:02X}").expect("fmt::Write on String cannot fail"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructs_abstract_in_order() {
        let inv = serde_json::json!({ "Wave": [0], "interference": [1], "memory": [2, 4], "holographic": [3] });
        let got = reconstruct_abstract(&inv).unwrap();
        assert_eq!(got, "Wave interference memory holographic memory");
    }

    #[test]
    fn percent_encodes_query() {
        assert_eq!(percent_encode("a b:c,d"), "a%20b%3Ac%2Cd");
    }

    #[test]
    fn importance_scales_with_citations_and_floors() {
        let mut w = Work {
            id: "x".into(), doi: None, title: "t".into(), year: None,
            cited_by_count: 0, authors: vec![], concepts: vec![],
            abstract_text: None, source: None,
        };
        assert!((w.ingest_importance() - 0.5).abs() < 1e-6);
        w.cited_by_count = 20_000;
        assert!(w.ingest_importance() > 0.9 && w.ingest_importance() <= 1.0);
    }
}
