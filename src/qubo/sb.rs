//! Simulated bifurcation (Goto, Tatsumura & Dixon 2019; the ballistic variant,
//! Goto et al. 2021) as a [`ConsolidationSolver`]: the quantum-inspired
//! classical solver that the 2026 field review found actually delivers
//! (Toshiba's third generation runs 2,000-spin Ising in real time), beside
//! [`ClassicalAnneal`](super::solver::ClassicalAnneal) under the same interface
//! contract (ADR-0038). Same seam, same budget, same provenance; a different
//! dynamics. Which one keeps more of what later mattered is T3.5's one-week
//! dream diff to answer, not this file.
//!
//! The dynamics, per spin `i` with position `x_i ∈ [-1, 1]` and momentum `y_i`:
//!
//! ```text
//!   y_i += [ -(a0 - a(t))·x_i + c0·( Σ_j J_ij·x_j + h_i ) ]·dt
//!   x_i += a0·y_i·dt
//!   if |x_i| > 1: x_i = sign(x_i), y_i = 0            (the "ballistic" wall)
//! ```
//!
//! with `a(t)` ramped linearly from 0 to `a0` over the run, so the system
//! bifurcates from the origin into a spin configuration. The QUBO is mapped to
//! Ising by `x_i = (1 + s_i)/2`; the best `sign(x)` seen during the run is the
//! answer, evaluated with the problem's own `energy`, so nothing here can
//! report an energy the objective does not agree with.

use std::time::Instant;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::entropy::{seed_from_bytes, EntropySource, PrngSource, Provenance};
use crate::qubo::problem::ConsolidationProblem;
use crate::qubo::solver::{
    ConsolidationSolution, ConsolidationSolver, SolveBudget, SolveError, EXACT_THRESHOLD,
};

/// Ballistic simulated bifurcation with restarts; exhaustive below
/// [`EXACT_THRESHOLD`], like the annealer, so small dreams are exact whatever
/// solver is configured.
#[derive(Debug, Clone)]
pub struct SimulatedBifurcation {
    seed: u64,
    provenance: Provenance,
    restarts: u32,
    /// Integration steps per restart.
    steps: u32,
    /// Time step. 0.5 to 1.0 is the usual range for bSB; smaller is safer on
    /// dense couplings.
    dt: f64,
}

impl Default for SimulatedBifurcation {
    fn default() -> Self {
        Self::from_entropy(&mut PrngSource::new())
    }
}

impl SimulatedBifurcation {
    /// Draw the seed from `src`, recording its provenance; on an entropy error
    /// fall back to the software PRNG and record that honestly, as the annealer does.
    pub fn from_entropy(src: &mut dyn EntropySource) -> Self {
        let (seed, provenance) = match src.draw(64) {
            Ok(d) => (seed_from_bytes(&d.bytes), d.provenance),
            Err(_) => {
                let d = PrngSource::new().draw(64).expect("PrngSource never fails");
                (seed_from_bytes(&d.bytes), d.provenance)
            }
        };
        Self {
            seed,
            provenance,
            restarts: 32,
            steps: 2000,
            dt: 0.5,
        }
    }

    /// Deterministic constructor for tests and reproduction.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            seed,
            provenance: Provenance::legacy(),
            restarts: 32,
            steps: 2000,
            dt: 0.5,
        }
    }

    pub fn with_schedule(mut self, restarts: u32, steps: u32, dt: f64) -> Self {
        self.restarts = restarts.max(1);
        self.steps = steps.max(1);
        self.dt = if dt.is_finite() && dt > 0.0 {
            dt.min(1.0)
        } else {
            0.5
        };
        self
    }

    /// The Ising form of the QUBO. With `x_i = (1 + s_i)/2`, minimising
    /// `Σ l_i x_i + Σ q_ij x_i x_j` is minimising `Σ h'_i s_i + Σ J'_ij s_i s_j`
    /// with `h'_i = l_i/2 + Σ_j q_ij/4` and `J'_ij = q_ij/4`. The bSB force is
    /// written for `E = -Σ J s s - Σ h s`, so `J = -J'` and `h = -h'`.
    fn ising(problem: &ConsolidationProblem) -> (Vec<Vec<f64>>, Vec<f64>) {
        let n = problem.num_vars();
        let mut j = vec![vec![0.0f64; n]; n];
        let mut h = vec![0.0f64; n];
        for (k, &c) in &problem.linear {
            if let Ok(i) = k.trim().parse::<usize>() {
                if i < n {
                    h[i] -= c / 2.0;
                }
            }
        }
        for (k, &c) in &problem.quadratic {
            if let Some((a, b)) = k.split_once(',') {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    if a < n && b < n && a != b {
                        j[a][b] -= c / 4.0;
                        j[b][a] -= c / 4.0;
                        h[a] -= c / 4.0;
                        h[b] -= c / 4.0;
                    }
                }
            }
        }
        (j, h)
    }

    fn exhaustive(problem: &ConsolidationProblem) -> (Vec<bool>, f64) {
        let n = problem.num_vars();
        let mut best = vec![false; n];
        let mut best_e = f64::INFINITY;
        for mask in 0u64..(1u64 << n) {
            let x: Vec<bool> = (0..n).map(|i| (mask >> i) & 1 == 1).collect();
            let e = problem.energy(&x);
            if e < best_e {
                best_e = e;
                best = x;
            }
        }
        (best, best_e)
    }

    /// One bSB run from a small random start; returns the best `sign(x)` seen.
    fn bifurcate_once(
        &self,
        problem: &ConsolidationProblem,
        j: &[Vec<f64>],
        h: &[f64],
        rng: &mut ChaCha8Rng,
    ) -> (Vec<bool>, f64) {
        let n = problem.num_vars();
        let a0 = 1.0f64;
        // c0 = 0.5 / (σ_J · √n): the standard normalisation; σ_J from the couplings.
        let mut sum_sq = 0.0;
        let mut cnt = 0usize;
        for row in j {
            for &v in row {
                if v != 0.0 {
                    sum_sq += v * v;
                    cnt += 1;
                }
            }
        }
        let sigma = if cnt > 0 {
            (sum_sq / cnt as f64).sqrt()
        } else {
            1.0
        };
        let c0 = 0.5 / (sigma.max(1e-12) * (n as f64).sqrt());

        let mut x: Vec<f64> = (0..n).map(|_| rng.gen_range(-0.1..0.1)).collect();
        let mut y: Vec<f64> = (0..n).map(|_| rng.gen_range(-0.1..0.1)).collect();
        let mut best = vec![false; n];
        let mut best_e = f64::INFINITY;
        let mut field = vec![0.0f64; n];

        for step in 0..self.steps {
            let a_t = a0 * step as f64 / self.steps as f64;
            for i in 0..n {
                let mut f = h[i];
                for k in 0..n {
                    f += j[i][k] * x[k];
                }
                field[i] = f;
            }
            for i in 0..n {
                y[i] += (-(a0 - a_t) * x[i] + c0 * field[i]) * self.dt;
                x[i] += a0 * y[i] * self.dt;
                if x[i].abs() > 1.0 {
                    x[i] = x[i].signum();
                    y[i] = 0.0;
                }
            }
            let cand: Vec<bool> = x.iter().map(|&v| v > 0.0).collect();
            let e = problem.energy(&cand);
            if e < best_e {
                best_e = e;
                best = cand;
            }
        }
        (best, best_e)
    }
}

impl ConsolidationSolver for SimulatedBifurcation {
    fn name(&self) -> &str {
        "SimulatedBifurcation"
    }

    fn solve(
        &self,
        problem: &ConsolidationProblem,
        budget: SolveBudget,
    ) -> Result<ConsolidationSolution, SolveError> {
        problem.validate().map_err(SolveError::Invalid)?;
        let n = problem.num_vars();
        if n == 0 {
            return Ok(ConsolidationSolution {
                assignment: Vec::new(),
                energy: 0.0,
                solver: self.name().to_string(),
                exact: true,
                samples: None,
                provenance: self.provenance.clone(),
            });
        }
        if n < EXACT_THRESHOLD {
            let (assignment, energy) = Self::exhaustive(problem);
            return Ok(ConsolidationSolution {
                assignment,
                energy,
                solver: self.name().to_string(),
                exact: true,
                samples: None,
                provenance: self.provenance.clone(),
            });
        }
        let (j, h) = Self::ising(problem);
        let start = Instant::now();
        let mut best: Vec<bool> = Vec::new();
        let mut best_e = f64::INFINITY;
        let mut samples: Vec<(Vec<bool>, f64, u32)> = Vec::new();
        for r in 0..self.restarts {
            let mut rng = ChaCha8Rng::seed_from_u64(
                self.seed ^ (r as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            );
            let (x, e) = self.bifurcate_once(problem, &j, &h, &mut rng);
            samples.push((x.clone(), e, 1));
            if e < best_e {
                best_e = e;
                best = x;
            }
            if start.elapsed() >= budget.wall_time {
                break;
            }
        }
        Ok(ConsolidationSolution {
            assignment: best,
            energy: best_e,
            solver: self.name().to_string(),
            exact: false,
            samples: Some(samples),
            provenance: self.provenance.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qubo::problem::{ProblemBuilder, VarKind};
    use crate::qubo::solver::ClassicalAnneal;
    use std::time::Duration;

    fn budget() -> SolveBudget {
        SolveBudget {
            wall_time: Duration::from_secs(5),
            ..Default::default()
        }
    }

    /// A random dense problem big enough to skip the exhaustive path, built the
    /// same way every time.
    fn random_problem(n: usize, seed: u64) -> ConsolidationProblem {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut b = ProblemBuilder::new("sb-test");
        let vars: Vec<_> = (0..n)
            .map(|i| b.add_variable(VarKind::Merge, &format!("v{i}")))
            .collect();
        for &v in &vars {
            b.set_linear(v, rng.gen_range(-1.0..1.0));
        }
        for a in 0..n {
            for c in (a + 1)..n {
                if rng.gen_bool(0.5) {
                    b.add_quadratic(vars[a], vars[c], rng.gen_range(-2.0..2.0));
                }
            }
        }
        b.build()
    }

    /// Exhaustive minimum over dense arrays (the problem's own `energy` parses
    /// string keys on every call, far too slow for 2^22 evaluations).
    fn brute(problem: &ConsolidationProblem) -> f64 {
        let n = problem.num_vars();
        let mut lin = vec![0.0f64; n];
        let mut quad: Vec<(usize, usize, f64)> = Vec::new();
        for (k, &c) in &problem.linear {
            lin[k.parse::<usize>().unwrap()] = c;
        }
        for (k, &c) in &problem.quadratic {
            let (a, b) = k.split_once(',').unwrap();
            quad.push((a.parse().unwrap(), b.parse().unwrap(), c));
        }
        let mut best = f64::INFINITY;
        for m in 0u64..(1u64 << n) {
            let mut e = 0.0;
            for (i, &l) in lin.iter().enumerate() {
                if (m >> i) & 1 == 1 {
                    e += l;
                }
            }
            for &(a, b, c) in &quad {
                if (m >> a) & 1 == 1 && (m >> b) & 1 == 1 {
                    e += c;
                }
            }
            if e < best {
                best = e;
            }
        }
        best
    }

    fn range(problem: &ConsolidationProblem) -> f64 {
        // A scale for "close": the sum of every coefficient's magnitude.
        problem
            .linear
            .values()
            .chain(problem.quadratic.values())
            .map(|c| c.abs())
            .sum::<f64>()
    }

    #[test]
    fn ising_map_preserves_the_ordering_of_assignments() {
        // Ising energy from the map must be QUBO energy up to one constant.
        let p = random_problem(6, 3);
        let (j, h) = SimulatedBifurcation::ising(&p);
        let n = p.num_vars();
        let mut offsets = Vec::new();
        for m in 0u64..(1u64 << n) {
            let x: Vec<bool> = (0..n).map(|i| (m >> i) & 1 == 1).collect();
            let s: Vec<f64> = x.iter().map(|&b| if b { 1.0 } else { -1.0 }).collect();
            let mut e_ising = 0.0;
            for i in 0..n {
                e_ising -= h[i] * s[i];
                for k in 0..n {
                    e_ising -= 0.5 * j[i][k] * s[i] * s[k];
                }
            }
            offsets.push(p.energy(&x) - e_ising);
        }
        let (lo, hi) = offsets
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| {
                (a.min(v), b.max(v))
            });
        assert!(
            hi - lo < 1e-9,
            "QUBO − Ising should be a constant, spread {}",
            hi - lo
        );
    }

    #[test]
    fn lands_within_a_small_gap_of_the_optimum_the_exhaustive_path_can_check() {
        // n just above EXACT_THRESHOLD so the dynamics actually run, but brute
        // force still knows the answer. A heuristic is not promised the optimum;
        // it is held to within 2% of the coefficient scale of it, every seed.
        let n = EXACT_THRESHOLD + 2;
        for seed in 1..=3u64 {
            let p = random_problem(n, seed);
            let sol = SimulatedBifurcation::with_seed(seed)
                .solve(&p, budget())
                .unwrap();
            assert!(!sol.exact);
            assert_eq!(
                sol.energy,
                p.energy(&sol.assignment),
                "reported energy is the objective's"
            );
            let opt = brute(&p);
            let gap = sol.energy - opt;
            assert!(gap >= -1e-9, "below the true optimum is impossible");
            assert!(
                gap <= 0.02 * range(&p),
                "seed {seed}: SB {} vs optimum {opt} (gap {gap})",
                sol.energy
            );
        }
    }

    #[test]
    fn at_least_as_good_as_the_annealer_on_the_same_seeds() {
        let mut worse = 0;
        for seed in 1..=8u64 {
            let p = random_problem(EXACT_THRESHOLD + 4, seed);
            let sb = SimulatedBifurcation::with_seed(seed)
                .solve(&p, budget())
                .unwrap();
            let sa = ClassicalAnneal::with_seed(seed)
                .solve(&p, budget())
                .unwrap();
            if sb.energy > sa.energy + 1e-9 {
                worse += 1;
            }
        }
        assert!(worse <= 2, "SB was worse than SA on {worse} of 8 problems");
    }

    #[test]
    fn deterministic_under_a_seed_and_exact_below_threshold() {
        let p = random_problem(EXACT_THRESHOLD + 3, 42);
        let a = SimulatedBifurcation::with_seed(7)
            .solve(&p, budget())
            .unwrap();
        let b = SimulatedBifurcation::with_seed(7)
            .solve(&p, budget())
            .unwrap();
        assert_eq!(a.assignment, b.assignment);
        let small = random_problem(3, 1);
        let s = SimulatedBifurcation::with_seed(1)
            .solve(&small, budget())
            .unwrap();
        assert!(s.exact && s.energy == brute(&small));
        assert_eq!(s.solver, "SimulatedBifurcation");
        assert_eq!(s.provenance.source, "prng://legacy");
    }
}
