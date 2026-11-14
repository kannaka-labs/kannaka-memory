//! Which text encoder a store uses: ids, stamp equivalence and selection order.
//!
//! Always compiled, whether or not the `native-embed` feature is on, so a build
//! without the native embedder still reads a native store's stamp and says why
//! it cannot serve it.

use crate::encoding::{SimpleHashEncoder, TextEncoder};

/// The `.encoder` stamp of a store written by the native embedder.
pub const NATIVE_ENCODER_ID: &str = "kannaka:all-minilm-l6-v2:384";
/// The stamp of a store written through Ollama's `all-minilm`. The native
/// embedder runs the same model and returns the same vectors (cosine above
/// 0.9999 on 198 of 200 measured texts; the two exceptions are Korean and
/// Devanagari, see `native_embed`), so the two stamps name one vector space.
pub const OLLAMA_MINILM_ID: &str = "ollama:all-minilm:384";
/// Model name inside [`NATIVE_ENCODER_ID`].
pub const NATIVE_MODEL: &str = "all-minilm-l6-v2";
/// Ollama's name for the same model.
pub const OLLAMA_MINILM_MODEL: &str = "all-minilm";
/// Default address of a local Ollama. `127.0.0.1`, not `localhost`: on Windows
/// every request to `localhost` took about 2 s longer (measured in #1180).
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";
/// Set by the `kannaka` binary to the stamp of the encoder it selected, so
/// that `openclaw::make_pipeline` (the system's second pipeline) builds the
/// same one. Not a user setting.
pub const SELECTED_ENV: &str = "KANNAKA_ENCODER_SELECTED";

/// Is the native embedder compiled into this build?
pub const fn native_built() -> bool {
    cfg!(feature = "native-embed")
}

/// A `.encoder` stamp parsed back into a selection: `hash:<dim>:<seed>`,
/// `hash2:<dim>:<seed>`, `ollama:<model>:<dim>` or the native id. Returns
/// `(kind, model-or-seed, dim)` with kind `hash|hash2|ollama|native`; `None`
/// for a stamp this build does not recognise, so the caller keeps its own
/// selection and the mismatch guard still fires.
pub fn parse_encoder_stamp(stamp: &str) -> Option<(String, String, usize)> {
    let parts: Vec<&str> = stamp.trim().split(':').collect();
    match parts.as_slice() {
        ["hash" | "hash2", dim, seed] => {
            let kind = parts[0].to_string();
            let d: usize = dim.parse().ok()?;
            seed.parse::<u64>().ok()?;
            Some((kind, (*seed).to_string(), d))
        }
        ["ollama", model, dim] if !model.is_empty() => {
            Some(("ollama".to_string(), (*model).to_string(), dim.parse().ok()?))
        }
        ["kannaka", model, "384"] if *model == NATIVE_MODEL => {
            Some(("native".to_string(), NATIVE_MODEL.to_string(), 384))
        }
        _ => None,
    }
}

/// Is this stamp one of the two names for the all-MiniLM-L6-v2 space?
pub fn is_minilm_stamp(stamp: &str) -> bool {
    let s = stamp.trim();
    s == NATIVE_ENCODER_ID || s == OLLAMA_MINILM_ID
}

/// Do two stamps name the same vector space? Equal stamps do, and so do
/// [`NATIVE_ENCODER_ID`] and [`OLLAMA_MINILM_ID`]. Nothing else does.
pub fn stamps_equivalent(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    a == b || (is_minilm_stamp(a) && is_minilm_stamp(b))
}

/// Was this stamp written by a semantic (model) encoder, not a hash encoder?
pub fn stamp_is_semantic(stamp: &str) -> bool {
    let s = stamp.trim();
    s.starts_with("ollama:") || s == NATIVE_ENCODER_ID
}

/// Do two embedding-model names (or stamps) name the same vector space? Used
/// by `collective::merge_guard`, which is handed bare model names.
pub fn embedding_models_equivalent(a: &str, b: &str) -> bool {
    fn canon(m: &str) -> &str {
        let m = m.trim();
        if m == OLLAMA_MINILM_MODEL || m == NATIVE_MODEL || is_minilm_stamp(m) {
            NATIVE_MODEL
        } else {
            m
        }
    }
    canon(a) == canon(b)
}

/// An encoder family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    Hash,
    Hash2,
    Ollama,
    Native,
}

/// Why [`select`] chose what it chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// `KANNAKA_ENCODER`, or an `[encoder] kind` other than `auto`.
    Explicit,
    /// The store's stamp names a hash encoder or another Ollama model.
    Stamp,
    /// The store was written through Ollama `all-minilm` and Ollama answers.
    OllamaReachable,
    /// The store was written through Ollama `all-minilm`, Ollama does not
    /// answer, and the native embedder returns the same vectors.
    OllamaUnreachable,
    /// The store was written by the native embedder.
    NativeStamp,
    /// No store yet and nothing configured.
    NewStore,
}

/// What [`select`] is given. Everything is a plain value so the order can be
/// tested without an environment, a config file or a network.
#[derive(Debug, Clone)]
pub struct SelectInput<'a> {
    /// `KANNAKA_ENCODER`, when set and not empty.
    pub env_kind: Option<&'a str>,
    /// `[encoder] kind` from config (`auto` when the section is absent).
    pub cfg_kind: &'a str,
    /// `KANNAKA_ENCODER_MODEL` or `[encoder] model`.
    pub model: &'a str,
    /// `KANNAKA_ENCODER_DIM` or `[encoder] dim`.
    pub dim: usize,
    /// The store's `.encoder` stamp, when it has one.
    pub stamp: Option<&'a str>,
    /// Is the native embedder compiled in ([`native_built`])?
    pub native_built: bool,
}

/// The encoder to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub kind: EncoderKind,
    /// Model name (Ollama, native); unused for the hash kinds.
    pub model: String,
    pub dim: usize,
    /// Seed of the hash kinds.
    pub seed: u64,
    /// The store's stamp overrode what config alone would have selected.
    pub adopted: bool,
    pub reason: Reason,
}

impl Selection {
    /// The stamp a new store written by this encoder gets.
    pub fn stamp(&self) -> String {
        match self.kind {
            EncoderKind::Hash => format!("hash:{}:{}", self.dim, self.seed),
            EncoderKind::Hash2 => format!("hash2:{}:{}", self.dim, self.seed),
            EncoderKind::Ollama => format!("ollama:{}:{}", self.model, self.dim),
            EncoderKind::Native => NATIVE_ENCODER_ID.to_string(),
        }
    }
}

fn parse_kind(kind: &str) -> Result<Option<EncoderKind>, String> {
    match kind.trim() {
        // An empty `[encoder] kind` has always meant the hash encoder.
        "hash" | "" => Ok(Some(EncoderKind::Hash)),
        "hash2" => Ok(Some(EncoderKind::Hash2)),
        "ollama" => Ok(Some(EncoderKind::Ollama)),
        "native" => Ok(Some(EncoderKind::Native)),
        "auto" => Ok(None),
        other => Err(format!("unknown encoder kind '{other}' (expected auto|native|ollama|hash|hash2)")),
    }
}

/// Selection order.
///
/// 1. `KANNAKA_ENCODER` wins outright. The stamp guard then decides whether
///    that encoder may open the store.
/// 2. Otherwise a store stamped with a hash encoder or another Ollama model is
///    opened with exactly that (its stamp is adopted, as before #1180).
/// 3. Otherwise a store in the all-MiniLM-L6-v2 space follows an explicit
///    `[encoder] kind` of `ollama` or `native`. With `auto`: a native stamp
///    uses native; an `ollama:all-minilm:384` stamp keeps using Ollama while it
///    answers (`ollama_reachable`, asked at most once) and uses native when it
///    does not.
/// 4. Otherwise (no store yet) an explicit `[encoder] kind` is used, and `auto`
///    means native, or Ollama when the configured model is not all-minilm at
///    384 or the native embedder is compiled out.
///
/// Never selects a hash encoder unless a stamp or an explicit setting names it.
pub fn select(input: &SelectInput, ollama_reachable: &mut dyn FnMut() -> bool) -> Result<Selection, String> {
    let cfg = parse_kind(input.cfg_kind)?;
    let make = |kind: EncoderKind, reason: Reason| match kind {
        EncoderKind::Native => Selection {
            kind,
            model: NATIVE_MODEL.to_string(),
            dim: 384,
            seed: 42,
            adopted: false,
            reason,
        },
        _ => Selection { kind, model: input.model.to_string(), dim: input.dim, seed: 42, adopted: false, reason },
    };
    let minilm_configured = input.model == OLLAMA_MINILM_MODEL && input.dim == 384;
    // What config alone selects, with no store to consult.
    let unstamped = match cfg {
        Some(kind) => make(kind, Reason::Explicit),
        None if input.native_built && minilm_configured => make(EncoderKind::Native, Reason::NewStore),
        None => make(EncoderKind::Ollama, Reason::NewStore),
    };

    if let Some(env) = input.env_kind {
        if let Some(kind) = parse_kind(env)? {
            return Ok(make(kind, Reason::Explicit));
        }
    }

    let Some((kind, arg, dim)) = input.stamp.and_then(parse_encoder_stamp) else {
        return Ok(unstamped);
    };
    let minilm = Selection {
        kind: EncoderKind::Ollama,
        model: OLLAMA_MINILM_MODEL.to_string(),
        dim: 384,
        seed: 42,
        adopted: false,
        reason: Reason::OllamaReachable,
    };
    let mut chosen = match kind.as_str() {
        "hash" | "hash2" => Selection {
            kind: if kind == "hash" { EncoderKind::Hash } else { EncoderKind::Hash2 },
            model: input.model.to_string(),
            dim,
            seed: arg.parse().unwrap_or(42),
            adopted: false,
            reason: Reason::Stamp,
        },
        "ollama" if !(arg == OLLAMA_MINILM_MODEL && dim == 384) => Selection {
            kind: EncoderKind::Ollama,
            model: arg,
            dim,
            seed: 42,
            adopted: false,
            reason: Reason::Stamp,
        },
        // The all-MiniLM-L6-v2 space, under either of its stamps.
        _ => match cfg {
            Some(EncoderKind::Ollama) => Selection { reason: Reason::Explicit, ..minilm },
            Some(EncoderKind::Native) => make(EncoderKind::Native, Reason::Explicit),
            _ if kind == "native" => make(EncoderKind::Native, Reason::NativeStamp),
            _ if !input.native_built => minilm,
            _ if ollama_reachable() => minilm,
            _ => make(EncoderKind::Native, Reason::OllamaUnreachable),
        },
    };
    chosen.adopted = !stamps_equivalent(&chosen.stamp(), &unstamped.stamp());
    Ok(chosen)
}

/// The native embedder as a boxed encoder, or why this build has none.
pub fn native_encoder() -> Result<Box<dyn TextEncoder>, String> {
    #[cfg(feature = "native-embed")]
    {
        Ok(Box::new(crate::native_embed::NativeEncoder::shared()))
    }
    #[cfg(not(feature = "native-embed"))]
    {
        Err("this build has no native text encoder (compiled without the `native-embed` feature)".to_string())
    }
}

/// Make sure the native embedder's weights are on disk and verified,
/// downloading them if needed. Returns where they are.
pub fn ensure_native_weights() -> Result<std::path::PathBuf, String> {
    #[cfg(feature = "native-embed")]
    {
        crate::native_embed::ensure_weights()
    }
    #[cfg(not(feature = "native-embed"))]
    {
        Err("this build has no native text encoder (compiled without the `native-embed` feature)".to_string())
    }
}

/// The encoder a stamp names, when it is one the 384-wide codebook can take
/// and needs no address: a hash encoder or the native embedder. Used by
/// `openclaw::make_pipeline` to mirror the store pipeline's selection.
pub(crate) fn addressless_encoder(stamp: &str, dim: usize) -> Option<Result<Box<dyn TextEncoder>, String>> {
    match parse_encoder_stamp(stamp)? {
        (kind, seed, d) if (kind == "hash" || kind == "hash2") && d == dim => {
            let seed = seed.parse().unwrap_or(42);
            Some(Ok(if kind == "hash" {
                Box::new(SimpleHashEncoder::new(d, seed))
            } else {
                Box::new(SimpleHashEncoder::new_cleaned(d, seed))
            }))
        }
        (kind, _, d) if kind == "native" && d == dim => Some(native_encoder()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(env: Option<&'a str>, cfg: &'a str, stamp: Option<&'a str>) -> SelectInput<'a> {
        SelectInput { env_kind: env, cfg_kind: cfg, model: "all-minilm", dim: 384, stamp, native_built: true }
    }

    /// Runs `select`, recording whether Ollama was probed.
    fn run(i: &SelectInput, reachable: bool) -> (Selection, bool) {
        let mut probed = false;
        let s = select(i, &mut || {
            probed = true;
            reachable
        })
        .expect("selects");
        (s, probed)
    }

    #[test]
    fn stamp_equivalence_is_exactly_the_minilm_pair() {
        assert!(stamps_equivalent(NATIVE_ENCODER_ID, OLLAMA_MINILM_ID));
        assert!(stamps_equivalent(OLLAMA_MINILM_ID, NATIVE_ENCODER_ID));
        assert!(stamps_equivalent(" ollama:all-minilm:384\n", NATIVE_ENCODER_ID));
        assert!(stamps_equivalent("hash:384:42", "hash:384:42"));
        for (a, b) in [
            (NATIVE_ENCODER_ID, "hash:384:42"),
            (NATIVE_ENCODER_ID, "hash2:384:42"),
            (NATIVE_ENCODER_ID, "ollama:mxbai-embed-large:1024"),
            (NATIVE_ENCODER_ID, "ollama:all-minilm:768"),
            (NATIVE_ENCODER_ID, "ollama:nomic-embed-text:384"),
            (OLLAMA_MINILM_ID, "hash:384:42"),
            ("hash:384:42", "hash2:384:42"),
            ("hash:384:42", "hash:384:7"),
            (NATIVE_ENCODER_ID, "kannaka:all-minilm-l12-v2:384"),
            (NATIVE_ENCODER_ID, ""),
        ] {
            assert!(!stamps_equivalent(a, b), "{a:?} and {b:?} must not be equivalent");
        }
    }

    #[test]
    fn native_stamp_parses_and_unknown_kannaka_stamps_do_not() {
        assert_eq!(
            parse_encoder_stamp(NATIVE_ENCODER_ID),
            Some(("native".to_string(), NATIVE_MODEL.to_string(), 384))
        );
        for bad in ["kannaka:all-minilm-l6-v2:768", "kannaka:other:384", "kannaka::384", "kannaka:all-minilm-l6-v2"] {
            assert_eq!(parse_encoder_stamp(bad), None, "stamp {bad:?} must not parse");
        }
    }

    #[test]
    fn semantic_stamps() {
        assert!(stamp_is_semantic(NATIVE_ENCODER_ID));
        assert!(stamp_is_semantic(OLLAMA_MINILM_ID));
        assert!(stamp_is_semantic("ollama:mxbai-embed-large:1024"));
        assert!(!stamp_is_semantic("hash:384:42"));
        assert!(!stamp_is_semantic("hash2:384:42"));
        assert!(!stamp_is_semantic("unknown"));
    }

    #[test]
    fn model_names_for_merge_guard() {
        assert!(embedding_models_equivalent("all-minilm", "all-minilm-l6-v2"));
        assert!(embedding_models_equivalent(OLLAMA_MINILM_ID, NATIVE_ENCODER_ID));
        assert!(embedding_models_equivalent("all-minilm", NATIVE_ENCODER_ID));
        assert!(embedding_models_equivalent("mxbai-embed-large", "mxbai-embed-large"));
        assert!(!embedding_models_equivalent("all-minilm", "text-embedding-3-small"));
        assert!(!embedding_models_equivalent("all-minilm-l6-v2", "all-minilm-l12-v2"));
    }

    #[test]
    fn new_store_with_nothing_configured_is_native() {
        let (s, probed) = run(&input(None, "auto", None), false);
        assert_eq!((s.kind, s.reason), (EncoderKind::Native, Reason::NewStore));
        assert_eq!(s.stamp(), NATIVE_ENCODER_ID);
        assert!(!s.adopted);
        assert!(!probed, "a new store does not ask whether Ollama is up");
        // ...and it is native even when Ollama would have answered.
        assert_eq!(run(&input(None, "auto", None), true).0.kind, EncoderKind::Native);
    }

    #[test]
    fn new_store_without_the_native_build_or_with_another_model_is_ollama() {
        let mut i = input(None, "auto", None);
        i.native_built = false;
        assert_eq!(run(&i, false).0.kind, EncoderKind::Ollama);
        let mut i = input(None, "auto", None);
        i.model = "mxbai-embed-large";
        i.dim = 1024;
        let s = run(&i, false).0;
        assert_eq!(s.stamp(), "ollama:mxbai-embed-large:1024");
    }

    #[test]
    fn explicit_env_wins_over_stamp_and_config() {
        for (env, kind) in [
            ("native", EncoderKind::Native),
            ("ollama", EncoderKind::Ollama),
            ("hash", EncoderKind::Hash),
            ("hash2", EncoderKind::Hash2),
        ] {
            for stamp in [None, Some(OLLAMA_MINILM_ID), Some(NATIVE_ENCODER_ID), Some("hash:384:7")] {
                for cfg in ["auto", "ollama", "native", "hash"] {
                    let (s, probed) = run(&input(Some(env), cfg, stamp), false);
                    assert_eq!(s.kind, kind, "env {env} cfg {cfg} stamp {stamp:?}");
                    assert_eq!(s.reason, Reason::Explicit);
                    assert!(!probed);
                }
            }
        }
        // KANNAKA_ENCODER=auto is the same as not setting it.
        assert_eq!(run(&input(Some("auto"), "auto", None), false).0.reason, Reason::NewStore);
    }

    #[test]
    fn explicit_config_kind_wins_for_a_new_store_and_inside_the_minilm_space() {
        assert_eq!(run(&input(None, "ollama", None), false).0.kind, EncoderKind::Ollama);
        assert_eq!(run(&input(None, "hash", None), false).0.kind, EncoderKind::Hash);
        assert_eq!(run(&input(None, "native", None), false).0.kind, EncoderKind::Native);
        // `kind = "ollama"` keeps an Ollama store on Ollama even when it is down...
        let (s, probed) = run(&input(None, "ollama", Some(OLLAMA_MINILM_ID)), false);
        assert_eq!((s.kind, s.reason), (EncoderKind::Ollama, Reason::Explicit));
        assert!(!probed);
        // ...and may open a native store, because the vectors are the same.
        let s = run(&input(None, "ollama", Some(NATIVE_ENCODER_ID)), true).0;
        assert_eq!(s.stamp(), OLLAMA_MINILM_ID);
        // `kind = "native"` opens an Ollama all-minilm store natively without probing.
        let (s, probed) = run(&input(None, "native", Some(OLLAMA_MINILM_ID)), true);
        assert_eq!((s.kind, s.reason), (EncoderKind::Native, Reason::Explicit));
        assert!(!probed);
    }

    #[test]
    fn ollama_minilm_store_keeps_ollama_when_reachable_and_goes_native_when_not() {
        let (s, probed) = run(&input(None, "auto", Some(OLLAMA_MINILM_ID)), true);
        assert_eq!((s.kind, s.reason), (EncoderKind::Ollama, Reason::OllamaReachable));
        assert_eq!(s.stamp(), OLLAMA_MINILM_ID);
        assert!(probed && !s.adopted);

        let (s, probed) = run(&input(None, "auto", Some(OLLAMA_MINILM_ID)), false);
        assert_eq!((s.kind, s.reason), (EncoderKind::Native, Reason::OllamaUnreachable));
        assert!(probed && !s.adopted);
        assert!(stamps_equivalent(&s.stamp(), OLLAMA_MINILM_ID), "the guard must let it open the store");

        // Compiled out: stay on Ollama, never anything else.
        let mut i = input(None, "auto", Some(OLLAMA_MINILM_ID));
        i.native_built = false;
        let (s, probed) = run(&i, false);
        assert_eq!(s.kind, EncoderKind::Ollama);
        assert!(!probed);
    }

    #[test]
    fn native_store_stays_native_without_probing() {
        for reachable in [true, false] {
            let (s, probed) = run(&input(None, "auto", Some(NATIVE_ENCODER_ID)), reachable);
            assert_eq!((s.kind, s.reason), (EncoderKind::Native, Reason::NativeStamp));
            assert!(!probed);
        }
    }

    #[test]
    fn hash_and_other_model_stamps_are_adopted_as_before() {
        for cfg in ["auto", "ollama", "native"] {
            let (s, probed) = run(&input(None, cfg, Some("hash:384:7")), true);
            assert_eq!((s.kind, s.seed, s.dim), (EncoderKind::Hash, 7, 384), "cfg {cfg}");
            assert_eq!(s.stamp(), "hash:384:7");
            assert!(s.adopted && !probed);
            let s = run(&input(None, cfg, Some("hash2:384:42")), true).0;
            assert_eq!(s.kind, EncoderKind::Hash2);
            let s = run(&input(None, cfg, Some("ollama:mxbai-embed-large:1024")), false).0;
            assert_eq!(s.stamp(), "ollama:mxbai-embed-large:1024");
            assert_eq!(s.reason, Reason::Stamp);
            assert!(s.adopted);
        }
        // A hash config over an all-minilm store follows the store, as before.
        let s = run(&input(None, "hash", Some(OLLAMA_MINILM_ID)), true).0;
        assert_eq!(s.kind, EncoderKind::Ollama);
        assert!(s.adopted);
    }

    #[test]
    fn nothing_selects_a_hash_encoder_unasked() {
        for stamp in [None, Some(OLLAMA_MINILM_ID), Some(NATIVE_ENCODER_ID), Some("future:v9:512")] {
            for cfg in ["auto", "ollama", "native"] {
                for reachable in [true, false] {
                    for built in [true, false] {
                        let mut i = input(None, cfg, stamp);
                        i.native_built = built;
                        let s = run(&i, reachable).0;
                        assert!(
                            matches!(s.kind, EncoderKind::Ollama | EncoderKind::Native),
                            "cfg {cfg} stamp {stamp:?} reachable {reachable} built {built} selected {:?}",
                            s.kind
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_kinds_are_errors() {
        assert!(select(&input(Some("onnx"), "auto", None), &mut || false).is_err());
        assert!(select(&input(None, "onnx", None), &mut || false).is_err());
    }

    #[test]
    fn addressless_encoders_follow_the_stamp() {
        assert_eq!(addressless_encoder("hash:384:42", 384).unwrap().unwrap().embedding_dim(), 384);
        assert!(addressless_encoder("hash2:384:42", 384).is_some());
        assert!(addressless_encoder("hash:768:42", 384).is_none(), "wrong width for the codebook");
        assert!(addressless_encoder(OLLAMA_MINILM_ID, 384).is_none(), "Ollama needs an address");
        assert!(addressless_encoder("unknown", 384).is_none());
        let native = addressless_encoder(NATIVE_ENCODER_ID, 384).expect("native is addressless");
        assert_eq!(native.is_ok(), native_built());
    }
}
