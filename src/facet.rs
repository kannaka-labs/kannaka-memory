//! ADR-0049 facet decomposition — compound memory → atomic facet texts.
//!
//! The measured problem this solves: a compound memory (identity + place +
//! building-id + market + note, all superposed into one wavefront) is not in the
//! top-6 for `"where is Kannaka Labs"` at ANY energy exponent, while the same
//! fact stored atomically surfaces at rank 1, sim 0.763. Compound memories encode
//! to smeared wavefronts; atomic facts resonate. **Reach is an encoding property**
//! — no amount of re-ranking promotes what the scan never fetched.
//!
//! ## This function must stay PURE
//!
//! Decomposition is a pure function of content bytes: fixed sentence split, fixed
//! unicode boundaries, no clock, no RNG, no model. The #521 byte-identical
//! dream-determinism contract depends on it — a dream backfill must produce the
//! same facets on every run, on every node. An LLM extractor, if ever added, runs
//! ONCE at wake `remember`-time and persists its output; the dream path stays a
//! pure read.
//!
//! ## What is deliberately NOT decomposed
//!
//! - Perception streams (`HEAR:`, `audio:heard`, `[cannon:` fixtures) — these are
//!   telemetry, not knowledge, and splitting them multiplies noise.
//! - The `research:` corpus — externally-sourced abstracts, not lived context.
//! - Anything with fewer than two independent clauses — a single-clause memory is
//!   already atomic; "decomposing" it into one facet just duplicates it.
//!
//! ## Why splits never cross because/so/if/but
//!
//! A causal, conditional, or contrastive connective is the *load* of the
//! sentence. "The gate fails closed, **but** it is still forgeable" splits into
//! two facets that each assert the opposite of the memory's actual claim. Those
//! connectives bind; sentence boundaries separate.

/// Maximum facets minted per parent. The cost discipline in ADR-0049 is a
/// 1-core/6GB hub: every facet is a full wavefront in the scan, so this cap is a
/// RAM budget, not a style preference.
pub const MAX_FACETS_PER_PARENT: usize = 6;

/// Below this, a fragment is not a fact. Tuned to reject "Fixed." / "Shipped it."
const MIN_FACET_WORDS: usize = 5;

/// A leading pronoun means the fragment's subject lives in a sibling sentence.
/// We repair by prepending the parent subject; if that is unavailable, we drop.
const LEADING_PRONOUNS: &[&str] = &[
    "it", "its", "they", "them", "their", "this", "that", "these", "those", "he",
    "she", "his", "her", "we", "our", "i", "there", "here", "then", "also", "which",
];

/// Connectives that must never be split across — the clause on either side is
/// meaningless, or actively misleading, without its partner.
const BINDING_CONNECTIVES: &[&str] = &[
    "because", "so", "if", "but", "unless", "although", "though", "since",
    "therefore", "however", "whereas", "while",
];

/// Prefixes marking content that is telemetry or external corpus, not lived
/// context. Matched case-insensitively against the trimmed head of the content.
const EXCLUDED_PREFIXES: &[&str] = &[
    "hear:", "audio:heard", "[cannon:", "research:", "see:", "visual:",
];

/// Is facet decomposition enabled on the WRITE path?
/// `KANNAKA_FACET_DECOMPOSE` — **default OFF**, byte-identical when unset, per
/// the ADR-0048/0050 discipline of shipping the mechanism dark and enabling it
/// on measured evidence. Read-side resolution is always on but is a no-op until
/// facets exist, so an unset flag means the whole feature is inert.
pub fn decompose_enabled() -> bool {
    std::env::var("KANNAKA_FACET_DECOMPOSE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on"))
        .unwrap_or(false)
}

/// Serializes the tests that MUTATE `KANNAKA_FACET_DECOMPOSE`.
///
/// The flag is process-global and `cargo test` runs tests in parallel threads
/// inside one process, so a test asserting "flag OFF stores exactly one
/// wavefront" can be broken by a different test switching the flag ON at that
/// instant. That is what happened: `write_path_flag_default_off_then_on_...`
/// and `single_clause_content_is_never_decomposed_even_with_the_flag_on` fight
/// over the same variable. The race is invisible at `--test-threads 1` and
/// surfaces only when scheduling happens to interleave them, which makes it the
/// worst kind of red build — one that blames whichever PR last changed the test
/// count.
///
/// Every test that calls `set_var`/`remove_var` on this flag must hold this
/// guard for its whole body and leave the flag unset.
///
/// This does NOT make the flag safe in general: a test elsewhere that merely
/// *reads* the flag through `decompose_enabled` (any `absorb` does) can still
/// observe it ON while a flag test holds the lock. Closing that hole means
/// making the setting injectable rather than ambient, which is a real refactor
/// of every `decompose_enabled` call site and is not attempted here.
#[cfg(test)]
static FLAG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the flag lock, ignoring poisoning.
///
/// A panicking test poisons the mutex; without this every later flag test would
/// report a lock error instead of its own assertion, hiding the real failure
/// behind the first one.
#[cfg(test)]
pub(crate) fn lock_decompose_flag() -> std::sync::MutexGuard<'static, ()> {
    FLAG_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Decompose `content` into atomic facet texts.
///
/// Returns an empty vec when the content should not be decomposed at all —
/// excluded origin, too few clauses, or too few surviving quality facets. An
/// empty return is the normal case and means "store this as one wavefront".
///
/// Deterministic: same bytes in, same facets out, forever.
pub fn decompose(content: &str) -> Vec<String> {
    if !is_decomposable(content) {
        return Vec::new();
    }

    let subject = leading_subject(content);
    let mut facets: Vec<String> = Vec::new();

    for sentence in split_sentences(content) {
        if facets.len() >= MAX_FACETS_PER_PARENT {
            break;
        }
        if let Some(f) = qualify(sentence, subject.as_deref()) {
            // Exact-duplicate facets are pure cost: same wavefront, twice.
            if !facets.iter().any(|e| e == &f) {
                facets.push(f);
            }
        }
    }

    // One facet is not a decomposition — it is a copy of the parent.
    if facets.len() < 2 {
        return Vec::new();
    }
    facets
}

/// Origin + compound gate. Cheap checks first.
fn is_decomposable(content: &str) -> bool {
    let trimmed = content.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    let head: String = trimmed.chars().take(16).collect::<String>().to_lowercase();
    if EXCLUDED_PREFIXES.iter().any(|p| head.starts_with(p)) {
        return false;
    }
    // Compound test: at least two sentence-terminated clauses.
    split_sentences(content).count() >= 2
}

/// Split on sentence terminators followed by whitespace.
///
/// Requiring the whitespace is what makes decimals safe without a lookup table:
/// `v1.2`, `6ab.67`, `sim 0.763`, and `ADR-0049.` mid-token all lack the space,
/// so they never split. Abbreviations like `e.g. ` DO split — accepted, because
/// the rule stays one line and therefore stays deterministic across nodes.
fn split_sentences(content: &str) -> impl Iterator<Item = &str> {
    let mut out: Vec<&str> = Vec::new();
    let bytes = content.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' || c == b'!' || c == b'?' {
            // Consume a run of terminators ("?!", "...").
            let mut j = i + 1;
            while j < bytes.len() && matches!(bytes[j], b'.' | b'!' | b'?') {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] as char).is_whitespace() {
                if content.is_char_boundary(start) && content.is_char_boundary(j) {
                    out.push(&content[start..j]);
                }
                // Skip the whitespace run.
                let mut k = j;
                while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                    k += 1;
                }
                start = k;
                i = k;
                continue;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    if start < content.len() && content.is_char_boundary(start) {
        let tail = &content[start..];
        if !tail.trim().is_empty() {
            out.push(tail);
        }
    }
    out.into_iter()
}

/// Tokens that end a subject phrase: copulas, very common verbs, determiners and
/// prepositions. Deliberately a small fixed list — a POS tagger would be more
/// accurate and less deterministic, and determinism is the harder constraint.
const SUBJECT_TERMINATORS: &[&str] = &[
    // copulas / auxiliaries
    "is", "was", "are", "were", "be", "been", "being", "has", "have", "had",
    "will", "would", "can", "could", "may", "might", "must", "should", "does",
    "did", "do",
    // frequent predicate verbs in this corpus
    "holds", "runs", "sits", "added", "shipped", "fixed", "landed", "reached",
    "became", "made", "gets", "got", "degrades", "sharpens", "produces", "means",
    "gives", "takes", "gave", "gone",
    // determiners / prepositions — a subject never continues through these
    "the", "a", "an", "of", "in", "on", "at", "to", "for", "with", "from", "by",
    "that", "which", "and", "or",
];

/// The parent's subject, used to repair pronoun-leading facets.
///
/// Starts at a capitalised or hyphenated token (a name-like head) and then
/// absorbs following lowercase tokens until a terminator, capped at three.
/// Absorbing the tail matters: "Dream gravity is a poison" must yield
/// **"Dream gravity"**, not "Dream" — the bare head turns a claim about one
/// knob into a claim about dreaming itself, which is a different and false
/// statement. Found by running this on real corpus content.
fn leading_subject(content: &str) -> Option<String> {
    let first = split_sentences(content).next()?;
    let mut subj: Vec<&str> = Vec::new();
    for tok in first.split_whitespace() {
        let clean = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        if clean.is_empty() {
            break;
        }
        if subj.len() >= 3 {
            break;
        }
        let lower = clean.to_lowercase();
        if SUBJECT_TERMINATORS.contains(&lower.as_str()) {
            break;
        }
        let first_char = clean.chars().next()?;
        let name_like = first_char.is_uppercase() || clean.contains('-');
        if subj.is_empty() {
            // The head must look like a name; otherwise there is no subject to
            // borrow and a pronoun-leading facet is dropped rather than guessed.
            if !name_like {
                break;
            }
            subj.push(clean);
        } else {
            // Tail: absorb the rest of the noun phrase, whatever its case.
            subj.push(clean);
        }
    }
    if subj.is_empty() { None } else { Some(subj.join(" ")) }
}

/// Quality gate for one candidate facet. `None` = drop it.
fn qualify(sentence: &str, subject: Option<&str>) -> Option<String> {
    let s = sentence.trim().trim_end_matches(|c: char| c == '.' || c == '!' || c == '?');
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() < MIN_FACET_WORDS {
        return None;
    }

    // A fragment that is mostly identifiers or numbers is a handle, not a fact.
    let alpha_words = words
        .iter()
        .filter(|w| w.chars().any(|c| c.is_alphabetic()) && !is_bare_identifier(w))
        .count();
    if alpha_words * 2 < words.len() {
        return None;
    }

    // Never emit a fragment that begins with a binding connective: its meaning
    // lives in the clause it was severed from.
    let first_lower = words[0]
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if BINDING_CONNECTIVES.contains(&first_lower.as_str()) {
        return None;
    }

    // Pronoun-leading fragment: repair with the parent subject, or drop.
    if LEADING_PRONOUNS.contains(&first_lower.as_str()) {
        let subj = subject?;
        // "it holds the market" -> "Kannaka Labs holds the market"
        let rest = words[1..].join(" ");
        if rest.split_whitespace().count() < MIN_FACET_WORDS.saturating_sub(1) {
            return None;
        }
        return Some(format!("{subj} {rest}"));
    }

    Some(s.to_string())
}

/// A token that carries no lexical signal: a uuid, a hash, a version, a pure
/// number. Used to reject facets that are mostly machine handles.
fn is_bare_identifier(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| !c.is_alphanumeric());
    if t.is_empty() {
        return true;
    }
    let digits = t.chars().filter(|c| c.is_ascii_digit()).count();
    let hexish = t.chars().filter(|c| c.is_ascii_hexdigit() || *c == '-').count();
    digits * 2 >= t.len() || (t.len() >= 12 && hexish == t.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determinism_same_bytes_same_facets() {
        let c = "Kannaka Labs sits in the Deal District. The building id is 638. \
                 The market opens at nine each morning.";
        let a = decompose(c);
        let b = decompose(c);
        assert_eq!(a, b, "decomposition must be a pure function of content bytes");
        assert!(a.len() >= 2);
    }

    #[test]
    fn decimals_and_versions_never_split() {
        // The exact boundaries ADR-0049 calls out: v1.2 and 6ab.67 style tokens.
        let c = "The release is v1.2 and the reading was 6ab.67 in that run. \
                 Similarity reached 0.763 on the atomic control.";
        let facets = decompose(c);
        assert_eq!(facets.len(), 2, "got: {facets:?}");
        assert!(facets[0].contains("v1.2"), "v1.2 was split: {facets:?}");
        assert!(facets[0].contains("6ab.67"), "6ab.67 was split: {facets:?}");
        assert!(facets[1].contains("0.763"), "0.763 was split: {facets:?}");
    }

    #[test]
    fn never_splits_across_binding_connectives() {
        // "but" carries the whole claim: split it and each half asserts the
        // opposite of what the memory says.
        let c = "The privacy gate fails closed against unknown input. \
                 But it is still forgeable by a crafted claim.";
        let facets = decompose(c);
        assert!(
            !facets.iter().any(|f| f.to_lowercase().starts_with("but")),
            "emitted a fragment led by a binding connective: {facets:?}"
        );
    }

    #[test]
    fn pronoun_lead_is_repaired_with_the_parent_subject() {
        let c = "Kannaka Labs holds the northern market square. \
                 It also runs the escrow vault on the same block.";
        let facets = decompose(c);
        let repaired = facets.iter().find(|f| f.contains("escrow"));
        assert!(repaired.is_some(), "escrow facet dropped: {facets:?}");
        assert!(
            repaired.unwrap().starts_with("Kannaka Labs"),
            "pronoun lead not repaired: {facets:?}"
        );
    }

    #[test]
    fn subject_repair_keeps_the_whole_noun_phrase() {
        // Regression: an earlier version borrowed only the capitalised head, so
        // "Dream gravity is a poison. It degrades agreement." became
        // "Dream degrades agreement" — a claim about dreaming rather than about
        // the gravity knob. Decomposition must never invent a different claim.
        let c = "Dream gravity is an individual-recall knob and a swarm-coherence poison.                  It degrades agreement across the constellation badly.";
        let facets = decompose(c);
        let repaired = facets.iter().find(|f| f.contains("degrades agreement")).unwrap();
        assert!(
            repaired.starts_with("Dream gravity"),
            "subject truncated to its head — claim changed: {repaired:?}"
        );
    }

    #[test]
    fn perception_and_research_origins_are_excluded() {
        for c in [
            "HEAR: Synchrony from QueenSync | tempo=121.9 centroid=1.99. Second clause here now.",
            "audio:heard veryfast bright | 172bpm centroid 4.55kHz. Another clause follows here.",
            "[cannon:test_2aa2f84e] Test Video transcript. That is a good point actually now.",
            "research: attention mechanisms in transformers. A second sentence of abstract text.",
        ] {
            assert!(decompose(c).is_empty(), "should not decompose: {c}");
        }
    }

    #[test]
    fn single_clause_is_not_decomposed() {
        assert!(decompose("Kannaka Labs sits in the Deal District").is_empty());
        // One qualifying clause plus one too-short fragment is still not a
        // decomposition — it would just duplicate the parent.
        assert!(decompose("Kannaka Labs sits in the Deal District. Fixed.").is_empty());
    }

    #[test]
    fn bare_identifier_fragments_are_dropped() {
        let c = "The migration landed cleanly on the production database. \
                 8db26ff8 eb5c 4cbd a8c1 02296320760b 12 34. \
                 The rollback plan was never needed in the end.";
        let facets = decompose(c);
        assert!(
            !facets.iter().any(|f| f.contains("02296320760b")),
            "kept an identifier-only fragment: {facets:?}"
        );
    }

    #[test]
    fn facet_count_is_capped() {
        let mut c = String::new();
        for i in 0..20 {
            c.push_str(&format!("The subsystem number {i} reached a stable state today. "));
        }
        assert!(decompose(&c).len() <= MAX_FACETS_PER_PARENT);
    }

    #[test]
    fn duplicate_facets_are_not_minted_twice() {
        let c = "The relay restarted cleanly after the outage. \
                 The relay restarted cleanly after the outage. \
                 The dashboard confirmed the recovery within a minute.";
        let facets = decompose(c);
        let mut sorted = facets.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), facets.len(), "duplicate facets minted: {facets:?}");
    }

    #[test]
    fn unicode_content_does_not_panic_or_split_mid_char() {
        let c = "Le système a redémarré proprement après la panne — ça a pris trois minutes. \
                 Ξ est resté stable pendant toute la période concernée.";
        let facets = decompose(c);
        for f in &facets {
            assert!(f.chars().count() > 0);
        }
    }
}

#[cfg(test)]
mod real_corpus_tests {
    use super::*;

    /// Verbatim excerpts from the live medium (2026-08-01). Hand-written test
    /// strings prove the rules; only real content proves the rules were the
    /// right ones.
    const REAL: &[&str] = &[
        "2026-06-19 Oracle health fixes: freed disk (91%->89%); disabled the kannaka-staff growth-watcher that was firing ineffective lite dreams every tick. The hub has been stable since. Nothing else changed that day.",
        "ADR-0050 added an off-by-default temporal factor to recall, raising currently-true-fact retrieval from 0.4774 to 0.8503 MRR in the L8 benchmark. The flag ships at 0.0 so existing callers are byte-identical.",
        "Dream gravity is an individual-recall knob and a swarm-coherence poison. Concentrating amplitude on each node's strongest attractor sharpens its own recall. It degrades agreement across the constellation.",
    ];

    #[test]
    fn real_memories_decompose_into_sane_facets() {
        for c in REAL {
            let facets = decompose(c);
            println!("\n--- parent: {}...", &c[..60.min(c.len())]);
            for f in &facets {
                println!("    facet: {f}");
            }
            assert!(facets.len() >= 2, "real compound produced <2 facets: {c}");
            for f in &facets {
                assert!(
                    f.split_whitespace().count() >= MIN_FACET_WORDS,
                    "short facet slipped the gate: {f:?}"
                );
                assert!(!f.ends_with('.'), "trailing terminator not trimmed: {f:?}");
                // The decimal/version boundaries must survive on real text too.
                assert!(!f.trim().is_empty());
            }
        }
    }

    #[test]
    fn real_decimals_survive_in_corpus_text() {
        let facets = decompose(REAL[1]);
        let joined = facets.join(" | ");
        assert!(joined.contains("0.4774"), "0.4774 split: {facets:?}");
        assert!(joined.contains("0.8503"), "0.8503 split: {facets:?}");
        assert!(joined.contains("0.0"), "0.0 split: {facets:?}");
    }
}

// --- ADR-0049 step 3: read-side facet -> parent resolution -------------------
//
// The seam is deliberately POST-SCAN and non-mutating: resolution runs over the
// returned (id, score) list, never inside the medium scan. Pushing it into the
// scan was considered and rejected by the design review.
//
// Every recall producer routes through `resolve` -- `recall_against`,
// `recall_vector`, and `recall_against_ids` -- so an unresolved path is not
// something a caller can forget. The trait is the enforcement: a producer that
// wants to return results at all has to hand them through here.

use std::collections::HashSet;
use uuid::Uuid;

/// A recall result that can be rewritten from a facet to its parent.
pub trait Resolvable {
    fn result_id(&self) -> Uuid;
    fn result_score(&self) -> f32;
    /// Present the parent's identity and full context instead of the facet's.
    fn rewrite_to_parent(&mut self, parent_id: Uuid, parent_content: String);
}

impl Resolvable for crate::medium::types::Resonance {
    fn result_id(&self) -> Uuid {
        self.id
    }
    fn result_score(&self) -> f32 {
        self.resonance_strength
    }
    fn rewrite_to_parent(&mut self, parent_id: Uuid, parent_content: String) {
        self.id = parent_id;
        self.content = parent_content;
    }
}

impl Resolvable for crate::medium::types::ChiralResonance {
    fn result_id(&self) -> Uuid {
        self.id
    }
    fn result_score(&self) -> f32 {
        self.resonance_strength
    }
    fn rewrite_to_parent(&mut self, parent_id: Uuid, parent_content: String) {
        self.id = parent_id;
        self.content = parent_content;
    }
}

/// How much to over-fetch before resolving. A parent can contribute up to
/// `MAX_FACETS_PER_PARENT` rows to the raw pool, all of which collapse to one
/// result -- so fetching only `top_k` would return far fewer than `top_k`
/// distinct memories after dedup.
pub fn overfetch_pool(top_k: usize) -> usize {
    top_k.saturating_mul(MAX_FACETS_PER_PARENT).max(top_k)
}

/// Resolve facets to parents, dedup by canonical id keeping the best score, and
/// truncate to `top_k`.
///
/// `parent_of` maps a facet id to its parent's `(id, content)`, returning `None`
/// for anything that is not a facet OR whose parent is missing.
///
/// **On a resolution miss the facet is surfaced as its own result** -- never
/// dropped, never attributed to a parent that is not there. A dangling facet is
/// still a true fragment of something the agent stored; silently discarding it
/// would lose data, and inventing a parent would fabricate provenance.
///
/// **Observe-once falls out of this contract.** Callers observe the list this
/// returns, which holds exactly one entry per surfaced constellation. Observing
/// the *raw* pool instead would inject energy once per facet -- an 8-facet
/// parent would take 8 injections and re-create precisely the rich-get-richer
/// bias ADR-0048 shipped to kill.
///
/// Input is assumed score-descending (every producer sorts before returning), so
/// the first occurrence of a canonical id is its highest score and dedup is a
/// stable first-wins pass. No re-sorting, so ordering stays deterministic.
pub fn resolve<T, F>(results: Vec<T>, top_k: usize, parent_of: F) -> Vec<T>
where
    T: Resolvable,
    F: Fn(Uuid) -> Option<(Uuid, String)>,
{
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(top_k.min(results.len()));
    let mut out: Vec<T> = Vec::with_capacity(top_k.min(results.len()));

    for mut r in results {
        if out.len() >= top_k {
            break;
        }
        let canonical = match parent_of(r.result_id()) {
            Some((pid, pcontent)) => {
                r.rewrite_to_parent(pid, pcontent);
                pid
            }
            None => r.result_id(),
        };
        if seen.contains(&canonical) {
            continue; // a sibling facet of an already-surfaced parent
        }
        seen.insert(canonical);
        out.push(r);
    }
    out
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use crate::medium::types::Resonance;

    fn res(id: Uuid, content: &str, score: f32) -> Resonance {
        Resonance {
            id,
            content: content.to_string(),
            similarity: score,
            resonance_strength: score,
            effective_strength: 1.0,
        }
    }

    #[test]
    fn sibling_facets_collapse_to_one_parent_keeping_best_score() {
        let parent = Uuid::new_v4();
        let (f1, f2, f3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pool = vec![
            res(f1, "facet one", 0.9),
            res(f2, "facet two", 0.7),
            res(f3, "facet three", 0.5),
        ];
        let pc = String::from("the whole compound memory");
        let out = resolve(pool, 10, |id| {
            if id == f1 || id == f2 || id == f3 {
                Some((parent, pc.clone()))
            } else {
                None
            }
        });

        assert_eq!(out.len(), 1, "siblings did not collapse");
        assert_eq!(out[0].id, parent);
        assert_eq!(
            out[0].content, "the whole compound memory",
            "parent context not restored"
        );
        assert!(
            (out[0].resonance_strength - 0.9).abs() < 1e-6,
            "kept a lower sibling score"
        );
    }

    #[test]
    fn resolution_miss_surfaces_the_facet_rather_than_dropping_it() {
        let orphan = Uuid::new_v4();
        let out = resolve(vec![res(orphan, "orphaned facet text", 0.8)], 10, |_| None);
        assert_eq!(out.len(), 1, "a dangling facet was silently dropped");
        assert_eq!(out[0].id, orphan);
        assert_eq!(out[0].content, "orphaned facet text");
    }

    #[test]
    fn observe_once_one_entry_per_constellation() {
        // The energy-bias trap: 8 facets of one parent must yield ONE row, so a
        // caller observing the returned list injects once, not eight times.
        let parent = Uuid::new_v4();
        let facets: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();
        let pool: Vec<Resonance> = facets
            .iter()
            .enumerate()
            .map(|(i, f)| res(*f, "f", 0.9 - i as f32 * 0.01))
            .collect();
        let pc = String::from("parent");
        let out = resolve(pool, 10, |_| Some((parent, pc.clone())));
        assert_eq!(out.len(), 1, "an 8-facet parent would take multiple injections");
    }

    #[test]
    fn unfaceted_results_pass_through_byte_identical() {
        // Until anything is decomposed, resolution must be a no-op. This is what
        // makes shipping step 3 before step 2 is wired provably safe.
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let pool: Vec<Resonance> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| res(*id, "plain", 1.0 - i as f32 * 0.1))
            .collect();
        let before: Vec<(Uuid, String, f32)> = pool
            .iter()
            .map(|r| (r.id, r.content.clone(), r.resonance_strength))
            .collect();
        let out = resolve(pool, 10, |_| None);
        let after: Vec<(Uuid, String, f32)> = out
            .iter()
            .map(|r| (r.id, r.content.clone(), r.resonance_strength))
            .collect();
        assert_eq!(before, after, "resolution perturbed an unfaceted corpus");
    }

    #[test]
    fn truncates_to_top_k_after_dedup_not_before() {
        // Fetching top_k raw and then deduping would return fewer than top_k.
        // Resolution must consume the over-fetched pool and still fill top_k.
        let p1 = Uuid::new_v4();
        let (a1, a2) = (Uuid::new_v4(), Uuid::new_v4());
        let (b, c) = (Uuid::new_v4(), Uuid::new_v4());
        let pool = vec![
            res(a1, "p1 facet a", 0.9),
            res(a2, "p1 facet b", 0.85),
            res(b, "distinct b", 0.8),
            res(c, "distinct c", 0.7),
        ];
        let pc = String::from("p1 parent");
        let out = resolve(pool, 2, |id| {
            if id == a1 || id == a2 {
                Some((p1, pc.clone()))
            } else {
                None
            }
        });
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, p1);
        assert_eq!(
            out[1].id, b,
            "dedup happened after truncation, losing a distinct memory"
        );
    }

    #[test]
    fn overfetch_pool_scales_with_the_facet_cap() {
        assert_eq!(overfetch_pool(10), 10 * MAX_FACETS_PER_PARENT);
        assert_eq!(overfetch_pool(0), 0);
    }
}
