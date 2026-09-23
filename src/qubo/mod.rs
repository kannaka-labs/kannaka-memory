//! ADR-0038 — consolidation as QUBO.
//!
//! The dream/consolidation phase's keep-link / merge / strengthen decision is a
//! combinatorial optimization (binary variables, pairwise interactions, a global
//! budget) — the QUBO/Ising class. This module freezes that problem at a
//! serialization boundary (`kannaka-qubo/1`) so the *solver* is swappable:
//! `ClassicalAnneal` today (T3.3), a quantum/QAOA subprocess later (T3.4),
//! without touching the engine.
//!
//! - [`problem`] — the `kannaka-qubo/1` format ([`ConsolidationProblem`]), the
//!   penalty-folding [`ProblemBuilder`], and the objective [`ConsolidationProblem::energy`].
//!   The emitter that turns a dream's merge plan into a problem also lives here.
//!
//! The solver trait + `ClassicalAnneal` (T3.3) land in a sibling `solver`
//! submodule; the engine's advisory re-score-before-apply path is T3.5. This
//! module ships only the boundary + emitter, so consolidation logic becomes
//! testable in isolation (golden QUBO files in `tests/qubo/`).

pub mod emit;
pub mod problem;
pub mod sb;
pub mod solver;

pub use problem::{
    dream_merge_problem, ConsolidationProblem, Constraint, MergeCandidate, Metadata, ProblemBuilder,
    VarKind, Variable, FORMAT,
};
pub use sb::SimulatedBifurcation;
pub use solver::{
    ClassicalAnneal, ConsolidationSolution, ConsolidationSolver, SolveBudget, SolveError,
    EXACT_THRESHOLD,
};
