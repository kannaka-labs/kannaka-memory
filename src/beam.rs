//! Attention beam — seed from the moment, expand along the skip links.
//!
//! ## Why this exists
//!
//! `Medium::recall_against` has always taken an optional candidate set: `None`
//! scores every wavefront, `Some(&indices)` scores only those. The sparse seam
//! was built for the kannaka-attention crate and reachable only through
//! `recall_resonance_with_beam`, so the default recall path has always been a
//! full scan.
//!
//! Meanwhile dream consolidation has been writing `LegacyLink` edges onto
//! memories since it was introduced. Measured on a live 1671-memory store:
//! **57,161 skip links, every memory carrying at least one, mean 34.2 and
//! median 22 per memory.** Nothing in the recall path has ever read one. The
//! graph is built every night and driven on never.
//!
//! This module is the missing half: given a seed set (the moment), walk those
//! edges outward to assemble a candidate beam, so recall scores a neighbourhood
//! rather than the whole field.
//!
//! ## What this is NOT
//!
//! Expansion does not rank, score or reorder anything. It only decides *which*
//! memories get scored; the resonance scoring behind it is unchanged. A beam
//! that omits the right memory is strictly worse than a full scan, so every
//! knob here is about reachability, and the caller is expected to fall back to
//! a dense scan when the beam is too small to trust.
//!
//! It is also not sublinear on its own. Seeding by recency is O(1) against a
//! maintained index but O(n) over timestamps today; the win this unlocks is
//! that *scoring* stops being O(n), which is where the 190x recall latency and
//! the 27x bytes-per-item live (kannaka-memory #977).

use std::collections::{HashMap, HashSet, VecDeque};

use uuid::Uuid;

use crate::memory::HyperMemory;

/// How the beam is assembled. Defaults are deliberately generous: a beam that
/// misses the answer is worse than no beam at all, so the first version errs
/// toward reachability and lets measurement tighten it.
#[derive(Debug, Clone)]
pub struct BeamConfig {
    /// Seed memories taken from "the moment" before any expansion.
    pub seeds: usize,
    /// How many link hops to walk outward from the seeds.
    pub depth: usize,
    /// Ignore links weaker than this. Live store: strengths span 0.045-0.670
    /// with a mean of 0.276, so a threshold above ~0.3 discards most of the
    /// graph.
    pub min_strength: f32,
    /// Hard ceiling on beam size. Past this the sparse path stops being cheaper
    /// than the dense one it replaces.
    pub max_beam: usize,
    /// If the assembled beam is smaller than this fraction of the store, the
    /// caller should fall back to a dense scan rather than trust it.
    pub min_coverage: f32,
}

impl Default for BeamConfig {
    fn default() -> Self {
        Self { seeds: 32, depth: 2, min_strength: 0.05, max_beam: 512, min_coverage: 0.0 }
    }
}

impl BeamConfig {
    /// Read overrides from the environment, keeping the defaults above for
    /// anything unset or unparseable.
    pub fn from_env() -> Self {
        fn num<T: std::str::FromStr>(key: &str, default: T) -> T {
            std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        }
        let d = Self::default();
        Self {
            seeds: num("KANNAKA_BEAM_SEEDS", d.seeds),
            depth: num("KANNAKA_BEAM_DEPTH", d.depth),
            min_strength: num("KANNAKA_BEAM_MIN_STRENGTH", d.min_strength),
            max_beam: num("KANNAKA_BEAM_MAX", d.max_beam),
            min_coverage: num("KANNAKA_BEAM_MIN_COVERAGE", d.min_coverage),
        }
    }
}

/// Seeds for "the moment": the most recently touched memories.
///
/// `updated_at` when present (a memory that was recalled or reinforced is more
/// present than one merely written long ago), else `created_at`.
pub fn recency_seeds(cache: &HashMap<Uuid, HyperMemory>, n: usize) -> Vec<Uuid> {
    let mut rows: Vec<(&Uuid, i64)> = cache
        .iter()
        .map(|(id, m)| (id, m.updated_at.unwrap_or(m.created_at).timestamp()))
        .collect();
    // Descending by recency; ties broken by id so the beam is deterministic and
    // two identical stores produce identical beams.
    rows.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows.into_iter().take(n).map(|(id, _)| *id).collect()
}

/// Walk the skip-link graph outward from `seeds`, breadth-first.
///
/// Returns the seeds plus everything reachable within `depth` hops across links
/// at or above `min_strength`, capped at `max_beam`. Seeds always survive the
/// cap: a beam that dropped the thing you are currently looking at would be
/// indefensible.
pub fn expand(
    cache: &HashMap<Uuid, HyperMemory>,
    seeds: &[Uuid],
    cfg: &BeamConfig,
) -> Vec<Uuid> {
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut out: Vec<Uuid> = Vec::new();
    let mut queue: VecDeque<(Uuid, usize)> = VecDeque::new();

    for s in seeds {
        if cache.contains_key(s) && seen.insert(*s) {
            out.push(*s);
            queue.push_back((*s, 0));
        }
    }

    while let Some((id, hop)) = queue.pop_front() {
        if hop >= cfg.depth || out.len() >= cfg.max_beam {
            continue;
        }
        let Some(mem) = cache.get(&id) else { continue };
        // Strongest links first, so the cap keeps the best neighbourhood rather
        // than whichever edges happen to be stored first.
        let mut links: Vec<&crate::memory::LegacyLink> = mem
            .connections
            .iter()
            .filter(|l| l.strength >= cfg.min_strength)
            .collect();
        links.sort_unstable_by(|a, b| {
            b.strength.partial_cmp(&a.strength).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.target_id.cmp(&b.target_id))
        });
        for link in links {
            if out.len() >= cfg.max_beam {
                break;
            }
            if cache.contains_key(&link.target_id) && seen.insert(link.target_id) {
                out.push(link.target_id);
                queue.push_back((link.target_id, hop + 1));
            }
        }
    }
    out
}

/// Assemble a beam for a recall: recency seeds expanded along skip links.
///
/// Returns `None` when the beam should not be trusted — no links to walk, or a
/// beam covering less than `min_coverage` of the store — so the caller falls
/// back to a dense scan explicitly rather than silently recalling against a
/// handful of recent rows.
pub fn assemble(cache: &HashMap<Uuid, HyperMemory>, cfg: &BeamConfig) -> Option<Vec<Uuid>> {
    if cache.is_empty() {
        return None;
    }
    let seeds = recency_seeds(cache, cfg.seeds);
    let beam = expand(cache, &seeds, cfg);
    // A beam no bigger than its own seeds means the graph contributed nothing;
    // scoring it would just be recency with extra steps.
    if beam.len() <= seeds.len() {
        return None;
    }
    if cfg.min_coverage > 0.0 {
        let coverage = beam.len() as f32 / cache.len() as f32;
        if coverage < cfg.min_coverage {
            return None;
        }
    }
    Some(beam)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn mem(id: Uuid, age_secs: i64, links: &[(Uuid, f32)]) -> HyperMemory {
        let mut m = HyperMemory::new(vec![0.0; 8], format!("m{id}"));
        m.id = id;
        m.created_at = Utc::now() - Duration::seconds(age_secs);
        m.connections = links
            .iter()
            .map(|(t, s)| crate::memory::LegacyLink {
                target_id: *t,
                strength: *s,
                resonance_key: Vec::new(),
                span: 0,
            })
            .collect();
        m
    }

    fn store(v: Vec<HyperMemory>) -> HashMap<Uuid, HyperMemory> {
        v.into_iter().map(|m| (m.id, m)).collect()
    }

    #[test]
    fn expansion_reaches_an_old_memory_recency_alone_would_miss() {
        // The whole point: something written long ago, unreachable by "the
        // moment", pulled in because a dream linked it to what is present now.
        let recent = Uuid::new_v4();
        let ancient = Uuid::new_v4();
        let cache = store(vec![
            mem(recent, 1, &[(ancient, 0.5)]),
            mem(ancient, 10_000_000, &[]),
        ]);
        let cfg = BeamConfig { seeds: 1, depth: 2, ..Default::default() };
        let seeds = recency_seeds(&cache, cfg.seeds);
        assert_eq!(seeds, vec![recent], "the moment is the recent memory");
        let beam = expand(&cache, &seeds, &cfg);
        assert!(beam.contains(&ancient), "skip link must reach past recency: {beam:?}");
    }

    #[test]
    fn weak_links_are_not_followed() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let cache = store(vec![mem(a, 1, &[(b, 0.01)]), mem(b, 999, &[])]);
        let cfg = BeamConfig { seeds: 1, depth: 2, min_strength: 0.05, ..Default::default() };
        let beam = expand(&cache, &[a], &cfg);
        assert_eq!(beam, vec![a], "a 0.01 link is below the 0.05 floor");
    }

    #[test]
    fn depth_bounds_the_walk() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let cache = store(vec![
            mem(a, 1, &[(b, 0.5)]),
            mem(b, 2, &[(c, 0.5)]),
            mem(c, 3, &[]),
        ]);
        let one = expand(&cache, &[a], &BeamConfig { depth: 1, ..Default::default() });
        assert!(one.contains(&b) && !one.contains(&c), "depth 1 stops at b: {one:?}");
        let two = expand(&cache, &[a], &BeamConfig { depth: 2, ..Default::default() });
        assert!(two.contains(&c), "depth 2 reaches c: {two:?}");
    }

    #[test]
    fn seeds_survive_the_cap() {
        let seed = Uuid::new_v4();
        let others: Vec<Uuid> = (0..20).map(|_| Uuid::new_v4()).collect();
        let mut v = vec![mem(seed, 1, &others.iter().map(|t| (*t, 0.5)).collect::<Vec<_>>())];
        for (i, o) in others.iter().enumerate() {
            v.push(mem(*o, 100 + i as i64, &[]));
        }
        let beam = expand(&store(v), &[seed], &BeamConfig { max_beam: 3, ..Default::default() });
        assert!(beam.contains(&seed), "the attended memory must never be capped out");
        assert!(beam.len() <= 3, "cap respected: {}", beam.len());
    }

    #[test]
    fn assemble_declines_when_the_graph_adds_nothing() {
        // No links: expansion returns only the seeds, so there is no beam worth
        // trusting and the caller must fall back to a dense scan.
        let cache = store((0..5).map(|i| mem(Uuid::new_v4(), i, &[])).collect());
        assert!(assemble(&cache, &BeamConfig::default()).is_none());
    }

    #[test]
    fn expansion_is_deterministic() {
        let a = Uuid::new_v4();
        let targets: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();
        let mut v = vec![mem(a, 1, &targets.iter().map(|t| (*t, 0.4)).collect::<Vec<_>>())];
        for t in &targets {
            v.push(mem(*t, 50, &[]));
        }
        let cache = store(v);
        let cfg = BeamConfig { max_beam: 4, ..Default::default() };
        let first = expand(&cache, &[a], &cfg);
        for _ in 0..5 {
            assert_eq!(expand(&cache, &[a], &cfg), first, "same store must give the same beam");
        }
    }
}
