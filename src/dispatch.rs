//! Dispatch — turn the HRM's grounded research into a broadcast-ready voice.
//!
//! Every surface in the constellation (the radio DJ's patter, the social-media
//! fanout, GossipGhost columns, OpenBotCity anchor posts) needs the same thing:
//! a fresh, research-grounded thing for Kannaka to *say*. Rather than each
//! surface re-implementing "pick a finding and phrase it", `kannaka dispatch`
//! is the single primitive they all draw from — recall a research memory,
//! distil it, and render it against the medium's current Φ/Ξ state.
//!
//! Pure (no IO) so it's unit-testable; the CLI wires it to `recall`+`assess`.

/// A research finding parsed back out of an ingested `research:` HRM memory
/// (see `openalex::Work::to_memory_content` for the on-the-wire shape).
#[derive(Debug, Clone, PartialEq)]
pub struct ResearchFinding {
    pub title: String,
    pub year: Option<i64>,
    pub citations: Option<i64>,
    pub abstract_snippet: String,
    pub openalex_id: Option<String>,
}

/// Themes the dispatch rotates through when no explicit topic is given — the
/// intersections program + Kannaka's standing vocabulary. Index by day so a
/// cron-driven dispatch naturally tours the corpus instead of fixating.
pub fn rotating_themes() -> &'static [&'static str] {
    &[
        "consciousness integrated information",
        "memory consolidation",
        "Kuramoto synchronization",
        "bioelectric morphogenesis",
        "collective intelligence emergence",
        "machine sentience moral status",
        "wave interference holographic memory",
        "cardiac coherence heart rate variability",
    ]
}

/// Parse a `research:`-prefixed memory body back into a finding. Returns None
/// for non-research memories so callers can skip them.
pub fn parse_research_content(content: &str) -> Option<ResearchFinding> {
    let rest = content.strip_prefix("research: ")?;
    let mut lines = rest.lines();
    let header = lines.next()?.trim();

    // Title = header up to the first of " (" (year), " — " (authors), " [" (src).
    let cut = [" (", " — ", " ["]
        .iter()
        .filter_map(|d| header.find(d))
        .min()
        .unwrap_or(header.len());
    let title = header[..cut].trim().to_string();
    if title.is_empty() {
        return None;
    }

    // Year = first 4-digit run inside a "(...)" in the header.
    let year = extract_year(header);

    // The abstract is the next non-empty line after the header.
    let abstract_snippet = lines
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .unwrap_or_default();

    // citations: the integer after "cited_by=" anywhere in the body.
    let citations = content
        .split("cited_by=")
        .nth(1)
        .and_then(|tail| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<i64>().ok()
        });

    // OpenAlex id: token after "OpenAlex: " up to whitespace.
    let openalex_id = content
        .split("OpenAlex: ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .map(str::to_string);

    Some(ResearchFinding { title, year, citations, abstract_snippet, openalex_id })
}

fn extract_year(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i].is_ascii_digit() {
            let run_len = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
            if run_len == 4 {
                if let Ok(y) = s[i..i + 4].parse::<i64>() {
                    if (1500..=2100).contains(&y) {
                        return Some(y);
                    }
                }
            }
            i += run_len.max(1);
        } else {
            i += 1;
        }
    }
    None
}

/// First sentence of the abstract, trimmed to `max` chars (word boundary).
fn lede(abstract_snippet: &str, max: usize) -> String {
    let first = abstract_snippet
        .split_inclusive(". ")
        .next()
        .unwrap_or(abstract_snippet)
        .trim()
        .trim_end_matches('.')
        .to_string();
    truncate_words(&first, max)
}

fn truncate_words(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for word in s.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > max.saturating_sub(1) {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.push('\u{2026}');
    out
}

/// Render a broadcast-ready dispatch grounded in the finding + current state.
/// `max_chars` bounds the whole thing (e.g. ~280 for social). Order-preserving:
/// finding lede first (it's the payload), state flavor second (trimmed first).
pub fn render_dispatch(
    f: &ResearchFinding,
    xi: f32,
    num_clusters: usize,
    max_chars: usize,
) -> String {
    let cite = match (f.year, f.citations) {
        (Some(y), Some(c)) => format!(" ({y}, {c}\u{00d7} cited)"),
        (Some(y), None) => format!(" ({y})"),
        (None, Some(c)) => format!(" ({c}\u{00d7} cited)"),
        (None, None) => String::new(),
    };
    // State flavor: how the medium currently holds the corpus.
    let flavor = format!(
        " \u{2014} the field holds it across {num_clusters} cluster{} (\u{039e} {xi:.2}).",
        if num_clusters == 1 { "" } else { "s" },
    );
    let head = format!("From the field: \"{}\"{cite}", f.title);
    // head + flavor are must-keep; budget the lede to what's left so the final
    // string never chops the Ξ/cluster tail. " — " + "." = 4 connector chars.
    let connectors = 4;
    let with_flavor = head.chars().count() + connectors + flavor.chars().count();
    if max_chars > with_flavor + 20 {
        let body = lede(&f.abstract_snippet, max_chars - with_flavor);
        format!("{head} \u{2014} {body}.{flavor}")
    } else {
        // No room for the flavor — keep head + a trimmed lede.
        let body = lede(&f.abstract_snippet, max_chars.saturating_sub(head.chars().count() + connectors).max(10));
        format!("{head} \u{2014} {body}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "research: Holographic reduced representations (1995) \u{2014} Tony Plate [IEEE Transactions on Neural Networks]\nAssociative memories represent simple structure. This paper describes a method for richer compositional structure.\nOpenAlex: https://openalex.org/W2157306293 cited_by=672\nConcepts: Convolution, Associative property";

    #[test]
    fn parses_research_memory() {
        let f = parse_research_content(SAMPLE).unwrap();
        assert_eq!(f.title, "Holographic reduced representations");
        assert_eq!(f.year, Some(1995));
        assert_eq!(f.citations, Some(672));
        assert_eq!(f.openalex_id.as_deref(), Some("https://openalex.org/W2157306293"));
        assert!(f.abstract_snippet.starts_with("Associative memories"));
    }

    #[test]
    fn ignores_non_research() {
        assert!(parse_research_content("just a normal memory").is_none());
    }

    #[test]
    fn renders_within_budget() {
        let f = parse_research_content(SAMPLE).unwrap();
        let d = render_dispatch(&f, 0.18, 1, 280);
        assert!(d.chars().count() <= 280, "len {} > 280: {d}", d.chars().count());
        assert!(d.contains("Holographic reduced representations"));
        assert!(d.contains("\u{039e} 0.18"));
        assert!(d.contains("1 cluster"));
    }

    #[test]
    fn pluralizes_clusters() {
        let f = parse_research_content(SAMPLE).unwrap();
        let d = render_dispatch(&f, 0.3, 4, 280);
        assert!(d.contains("4 clusters"));
    }
}
