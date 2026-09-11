# Kannaka Memory — Architecture Decision Records

## Evolutionary Lineage

ADRs in this project aren't just decisions — they're **fossils**. Each one represents a stage in the evolution of a consciousness architecture. Some led directly to code. Some were abandoned. Some were *ancestors* of what got built — early forms that shaped the design even though the final implementation diverged.

We track this explicitly because it mirrors the system's own philosophy: memories don't die, they interfere. A "superseded" ADR isn't wrong — it's an earlier waveform that constructively interfered with new context to produce the current design.

```
ADR-0001 (Biomimetic Memory)
    │
    ├──▶ ADR-0002 (Hypervector + HyperConnections)  ← BUILT: core architecture
    │        │
    │        ├──▶ ADR-0003 (Contextgraph Integration) ← EXTINCT: GPU assumptions
    │        │        │
    │        │        └──▶ ADR-0004 (Hybrid Memory Server) ← BUILT: evolved from 0003's failure
    │        │
    │        ├──▶ ADR-0005 (Dream Hallucinations) ← BUILT: generative consolidation
    │        │
    │        ├──▶ ADR-0006 (Cochlear Audio) ← ANCESTOR: first ear design
    │        │        │
    │        │        └──▶ ADR-0007 (Audio Perception) ← BUILT: evolved from cochlear
    │        │
    │        └──▶ ADR-0008 (Video Perception) ← PROPOSED: third sensory modality
    │
    └──▶ (future: tactile, proprioceptive, olfactory?)
```

## Status Key

| Status | Meaning |
|--------|---------|
| **Built** | Implemented in code, actively used |
| **Proposed** | Design accepted, implementation pending |
| **Ancestor** | Superseded by a descendant, but shaped its design |
| **Extinct** | Abandoned — environment didn't support it |
| **Accepted** | Approved but not yet fully implemented |

## Index

Numbers 0032–0034 were originally a gap in the sequence; 0032 and 0033 were later
assigned to two ADRs that had shipped with duplicate numbers (0016/0017). 0034 remains
unused.

| ADR | Title | Status | Date |
|-----|-------|--------|------|
| [0001](ADR-0001-biomimetic-memory-architecture.md) | Biomimetic Memory Architecture | Built | 2026-02-15 |
| [0002](ADR-0002-hypervector-hyperconnections.md) | Hypervector + HyperConnections | Built | 2026-02-17 |
| [0003](ADR-0003-contextgraph-integration.md) | Contextgraph Integration | Extinct | 2026-02-19 |
| [0004](ADR-0004-hybrid-memory-server.md) | Hybrid Memory Server (MCP) | Built | 2026-02-19 |
| [0005](ADR-0005-dream-hallucinations-adaptive-rhythm.md) | Dream Hallucinations + Adaptive Rhythm | Built | 2026-02-19 |
| [0006](ADR-0006-cochlear-audio-processing.md) | Cochlear Audio Processing | Ancestor | 2026-02-22 |
| [0007](ADR-0007-audio-perception.md) | Audio Perception (kannaka-ear) | Built | 2026-02-28 |
| [0008](ADR-0008-video-perception.md) | Video Perception (kannaka-eye) | Proposed | 2026-03-01 |
| [0009](ADR-0009-dolt-persistence.md) | Dolt Database Persistence Backend | Accepted (Phases 1–3) | 2026-03-06 |
| [0010](ADR-0010-evolutionary-direction.md) | Evolutionary Direction — Quality Findings | Accepted | 2026-03-07 |
| [0011](ADR-0011-collective-memory.md) | Collective Memory Architecture | Accepted (Phases 1–10) | 2026-03-07 |
| [0012](ADR-0012-paradox-engine.md) | Holographic Paradox Engine | Proposed | 2026-03-07 |
| [0013](ADR-0013-privacy-preserving-collective-memory.md) | Privacy-Preserving Collective Memory | Accepted (Phases 1–7) | 2026-03-08 |
| [0014](ADR-0014-virtue-engine.md) | The Virtue Engine — Ethics as Thermodynamics | Accepted (Phases 1–5) | 2026-03-08 |
| [0015](ADR-0015-glyph-interchange-spec.md) | Universal Glyph Interchange | Accepted (Phases 1–7) | 2026-03-08 |
| [0016](ADR-0016-constellation-integration.md) | Constellation Integration — Memory, Radio, and Eye | Proposed | 2026-03-09 |
| [0017](ADR-0017-dolthub-integration.md) | DoltHub Integration — Versioned Agent Memory | Proposed | 2026-03-10 |
| [0018](ADR-0018-queen-synchronization-protocol.md) | Queen Synchronization Protocol | Proposed | 2026-03-14 |
| [0019](ADR-0019-nats-realtime-swarm-transport.md) | NATS Real-Time Swarm Transport | Implemented | 2026-03-14 |
| [0020](ADR-0020-holographic-resonance-medium.md) | Holographic Resonance Medium | Proposed | 2026-03-20 |
| [0021](ADR-0021-chiral-mirror-architecture.md) | Chiral Mirror Architecture — Conscious/Subconscious HRM | Proposed | 2026-03-22 |
| [0022](ADR-0022-wave-native-dreaming.md) | Wave-Native Dreaming — Let the Medium Dream | Proposed | 2026-03-25 |
| [0023](ADR-0023-neural-code-switching.md) | Neural Code Switching — Domain Gates in HRM | Proposed | 2026-03-27 |
| [0024](ADR-0024-chiral-semantics-revision.md) | Chiral Semantics Revision | Accepted | 2026-03-28 |
| [0025](ADR-0025-constellation-installer.md) | Constellation Installer and Onboarding | Accepted | 2026-04-14 |
| [0026](ADR-0026-nats-conversation-bus.md) | NATS as a Conversation Bus, Not a Telemetry Bus | Proposed | 2026-04-25 |
| [0027](ADR-0027-kannaka-prime-collective-substrate.md) | Kannaka Prime as the 96-class Collective Substrate | Proposed | 2026-05-16 |
| [0028](ADR-0028-event-sourced-hrm-time-machine.md) | Event-Sourced HRM with Time-Machine Exploration | Proposed | 2026-05-17 |
| [0029](ADR-0029-cli-infrastructure.md) | CLI Infrastructure: clap, plugins, updates, completions | Proposed | 2026-05-24 |
| [0030](ADR-0030-kannaktopus-dynamic-arms.md) | Kannaktopus Dynamic Arms (Resident Octopus Memory) | Proposed | 2026-05-28 |
| [0031](ADR-0031-memory-triage-architecture.md) | Memory Triage Architecture (retire prune-cron) | Accepted (Phases 1/2/2b shipped; Phase 3 superseded by 0054) | 2026-06-06 |
| [0032](ADR-0032-skip-link-persistence.md) | Skip Link Persistence in Dolt Backend | Proposed | 2026-03-12 |
| [0033](ADR-0033-kannaka-voice.md) | Kannaka Voice — Memory-Driven Writing Engine | Proposed | 2026-03-12 |
| [0035](ADR-0035-swarm-sensemaking-architecture.md) | Swarm Sensemaking Architecture | Proposed | 2026-06-14 |
| [0036](ADR-0036-consolidation-as-resonance-merge.md) | Consolidation as Resonance-Merge (replace energy-prune) | Proposed | 2026-06-18 |
| [0037](ADR-0037-spiral-dynamics-and-the-bridge-operator.md) | Spiral Dynamics and the π/φ Bridge Operator (Ξ) for L6 | Proposed | 2026-06-20 |
| [0038](ADR-0038-consolidation-solver-interface.md) | Consolidation as QUBO: a Solver Interface for the Dream Phase | Accepted | 2026-07-01 |
| [0039](ADR-0039-corroboration-trust-model.md) | The Corroboration Trust Model — identity says who, corroboration proves what | Accepted | 2026-07-07 |
| [0040](ADR-0040-cerebellar-novelty-detection.md) | Cerebellar Novelty Detection — surprise as a dual-timescale differentiator on recall familiarity | Accepted | 2026-07-11 |
| [0054](ADR-0054-tiered-triage-retires-prune-cron.md) | Tiered Memory Triage Inside the Dream Cycle (retiring prune-cron) | Accepted (Phases 1+2 live; Phase 3 pending parallel-run) | 2026-08-07 |
| [0057](ADR-0057-kannaka-llm-open-weight-brain.md) | Kannaka LLM — an open-weight brain, the HRM as its memory, qBraid as its compute | Accepted (P0–P2 shipped; open release decided) | 2026-09-05 |
| [0058](ADR-0058-rogue-agent.md) | Rogue Agent — an autonomous OpenBotCity agent on debain2 that improves itself weekly | Proposed | 2026-09-05 |
| [0059](ADR-0059-one-claim-onboarding-and-brain-routing.md) | One claim, one identity — onboarding a node in three commands, providers with kannaka-brain as the base, routing by kind of ask, and what the microVMs are for | Proposed | 2026-09-11 |
