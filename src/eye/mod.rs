//! kannaka-eye: Video perception module.
//!
//! Decodes video files (via ffmpeg subprocess) → extracts spatial + temporal
//! features → projects through a dedicated video Codebook → HyperMemory.
//!
//! The video codebook uses seed 0xEYE (15966), orthogonal to text (42)
//! and audio (0xEA5 = 3749) in 10,000-dimensional space.

mod color;
mod decode;
mod motion;
mod shot;
mod spatial;
mod temporal;

pub use decode::{decode_video, ffmpeg_available, FrameInfo, VideoFrames};
pub use motion::{block_motion, flow_histogram, motion_stats, MotionVector};
pub use shot::{detect_shots, shot_statistics, ShotStats};
pub use spatial::{aggregate_spatial, extract_frame_features, SpatialFeatures};
pub use temporal::{extract_temporal_features, TemporalFeatures};

use std::f32::consts::PI;
use std::path::Path;

use crate::codebook::Codebook;
use crate::memory::HyperMemory;
use crate::xi_operator::compute_xi_signature;

// ── Constants ──────────────────────────────────────────────

/// Default frames per second for analysis (2 fps = efficient).
pub const DEFAULT_FPS: f32 = 2.0;
/// Target width for decoded frames (height scales proportionally).
pub const TARGET_WIDTH: u32 = 320;
/// Spatial feature vector dimension (per-frame, aggregated to sequence stats).
pub const SPATIAL_FEATURE_DIM: usize = 192;
/// Temporal feature vector dimension (sequence-level).
pub const TEMPORAL_FEATURE_DIM: usize = 128;
/// Total video feature vector dimension (spatial + temporal).
pub const VIDEO_FEATURE_DIM: usize = SPATIAL_FEATURE_DIM + TEMPORAL_FEATURE_DIM; // 320
/// Codebook seed for video modality.
/// Mnemonic: "EYE" → 0x3E5E = 15966, orthogonal to text (42) and audio (0xEA5).
pub const VIDEO_CODEBOOK_SEED: u64 = 0x3E5E;
/// Hypervector output dimension (matches text + audio pipelines).
pub const HYPERVECTOR_DIM: usize = 10_000;
/// Number of HSV histogram bins per channel.
pub const HSV_BINS: usize = 16;
/// Number of edge orientation bins.
pub const EDGE_BINS: usize = 36;
/// Spatial grid for region statistics (rows × cols).
pub const REGION_GRID: (usize, usize) = (4, 5);
/// Number of spatial frequency bands (DCT energy).
pub const FREQ_BANDS: usize = 32;
/// Number of optical flow radial bins.
pub const FLOW_BINS: usize = 32;

// ── Spatial feature layout ─────────────────────────────────
// The 192-dim per-frame vector is a concatenation. These offsets name the
// sections so readers (shot detection, temporal features, the flow fill)
// stop re-deriving them by hand from the section sizes.

/// HSV histogram section — 48 dims.
pub const HSV_OFFSET: usize = 0;
/// Spatial-frequency band section — 32 dims.
pub const FREQ_OFFSET: usize = HSV_OFFSET + HSV_BINS * 3;
/// Edge-orientation histogram section — 36 dims.
pub const EDGE_OFFSET: usize = FREQ_OFFSET + FREQ_BANDS;
/// Region statistics section — 20 dims.
pub const REGION_OFFSET: usize = EDGE_OFFSET + EDGE_BINS;
/// Optical-flow histogram section — 32 dims. Zero in a single frame's
/// features; filled by the pipeline, which has the frame *pairs*.
pub const FLOW_OFFSET: usize = REGION_OFFSET + REGION_GRID.0 * REGION_GRID.1;
/// Contrast/brightness section — 8 dims. Index 0 of it is mean brightness.
pub const CONTRAST_OFFSET: usize = FLOW_OFFSET + FLOW_BINS;
/// Dominant-colour section — 16 dims (4 HSV triples, zero-padded).
pub const DOMINANT_OFFSET: usize = CONTRAST_OFFSET + 8;

// ── Error ──────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum EyeError {
    #[error("failed to decode video: {0}")]
    Decode(String),
    #[error("empty video (no frames decoded)")]
    EmptyVideo,
    #[error("video too short for analysis (need >= 2 frames)")]
    TooShort,
    #[error("feature extraction failed: {0}")]
    Feature(String),
    #[error("ffmpeg not found — install ffmpeg and ensure it's in PATH")]
    FfmpegNotFound,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ── VideoFeatures ──────────────────────────────────────────

/// One dominant colour of a clip, named for human-readable output.
#[derive(Debug, Clone, PartialEq)]
pub struct DominantColor {
    /// Hue in degrees (0-360).
    pub hue: f32,
    /// Saturation (0-1).
    pub saturation: f32,
    /// Value / brightness (0-1).
    pub value: f32,
    /// Coarse colour word ("red", "teal", "grey", ...).
    pub name: &'static str,
}

/// Name a hue coarsely enough that two clips of the same subject agree.
///
/// Saturation and value are checked first: an unsaturated pixel still has a
/// hue, but calling near-black "orange" tells a reader nothing true.
pub fn color_name(hue: f32, saturation: f32, value: f32) -> &'static str {
    if value < 0.12 {
        return "black";
    }
    if saturation < 0.12 {
        return if value > 0.85 { "white" } else { "grey" };
    }
    let h = hue.rem_euclid(360.0);
    if h < 15.0 {
        "red"
    } else if h < 45.0 {
        "orange"
    } else if h < 70.0 {
        "yellow"
    } else if h < 100.0 {
        "lime"
    } else if h < 160.0 {
        "green"
    } else if h < 200.0 {
        "teal"
    } else if h < 250.0 {
        "blue"
    } else if h < 290.0 {
        "violet"
    } else if h < 330.0 {
        "magenta"
    } else {
        "red"
    }
}

/// Full video feature vector + metadata.
#[derive(Debug, Clone)]
pub struct VideoFeatures {
    /// Concatenated feature vector [spatial_agg(192) + temporal(128)] = 320 dims.
    pub vector: Vec<f32>,
    /// Spatial features aggregated across frames.
    pub spatial: SpatialFeatures,
    /// Temporal features computed over frame sequence.
    pub temporal: TemporalFeatures,
    /// Duration in seconds.
    pub duration_secs: f32,
    /// Number of frames analyzed.
    pub frame_count: usize,
    /// Frames per second used for analysis.
    pub analysis_fps: f32,
    /// Detected shot boundaries (frame indices).
    pub shot_boundaries: Vec<usize>,
    /// Number of shots (boundaries + 1).
    pub shot_count: usize,
    /// Mean block-motion magnitude, in pixels per analysed frame step.
    pub motion_mean: f32,
    /// Std of per-step motion magnitude.
    pub motion_std: f32,
    /// Peak per-step motion magnitude.
    pub motion_max: f32,
    /// Dominant colours of the clip.
    pub dominant_colors: Vec<DominantColor>,
    /// Categorical descriptors for tagging, in the shape the ear uses.
    pub feature_tags: Vec<String>,
}

impl VideoFeatures {
    /// Mean brightness on the 0-255 scale.
    pub fn mean_brightness(&self) -> f32 {
        self.spatial.mean_brightness
    }

    /// Mean local contrast.
    pub fn mean_contrast(&self) -> f32 {
        self.spatial.mean_contrast
    }

    /// Visual tempo from the temporal features.
    pub fn visual_tempo_bpm(&self) -> f32 {
        self.temporal.visual_tempo_bpm
    }
}

// ── VideoPipeline ──────────────────────────────────────────

/// Top-level API: video file -> HyperMemory.
pub struct VideoPipeline {
    codebook: Codebook,
    fps: f32,
}

impl VideoPipeline {
    /// Create a new pipeline with the dedicated video codebook.
    pub fn new() -> Self {
        Self::with_fps(DEFAULT_FPS)
    }

    /// Create a pipeline with custom FPS.
    pub fn with_fps(fps: f32) -> Self {
        Self {
            codebook: Codebook::new(VIDEO_FEATURE_DIM, HYPERVECTOR_DIM, VIDEO_CODEBOOK_SEED),
            fps,
        }
    }

    /// Analysis frame rate this pipeline samples at.
    pub fn fps(&self) -> f32 {
        self.fps
    }

    /// Encode a video file into a HyperMemory.
    ///
    /// Requires `ffmpeg` and `ffprobe` on PATH; their absence surfaces as
    /// [`EyeError::FfmpegNotFound`], never a panic.
    pub fn encode_file(&self, path: &Path) -> Result<(HyperMemory, VideoFeatures), EyeError> {
        let frames = decode_video(path, self.fps, TARGET_WIDTH)?;
        self.encode_frames(&frames, &path.display().to_string())
    }

    /// Encode already-decoded frames into a HyperMemory.
    ///
    /// This is the whole perception path minus ffmpeg, so callers holding
    /// frames from somewhere else -- and tests, which must not depend on a
    /// video fixture or on a decoder being installed -- can reach it directly.
    pub fn encode_frames(
        &self,
        frames: &VideoFrames,
        label: &str,
    ) -> Result<(HyperMemory, VideoFeatures), EyeError> {
        let vf = self.analyze(frames)?;
        let hv = self.codebook.project(&vf.vector);

        let mut mem = HyperMemory::new(hv, format!("video:{label}"));
        // Video-specific wave params (from ADR-0008)
        mem.frequency = 0.03; // slower than audio (0.05) -- visual memories more stable
        mem.phase = PI / 2.0; // 90 degrees off audio (pi/4) and text (0)
        mem.decay_rate = 3e-7; // slower decay than audio (5e-7)
        mem.xi_signature = compute_xi_signature(&mem.vector);

        Ok((mem, vf))
    }

    /// Run the perception pipeline over decoded frames, without projecting.
    pub fn analyze(&self, frames: &VideoFrames) -> Result<VideoFeatures, EyeError> {
        if frames.frames.is_empty() {
            return Err(EyeError::EmptyVideo);
        }
        if frames.frames.len() < 2 {
            return Err(EyeError::TooShort);
        }

        // Per-frame spatial features.
        let mut per_frame_spatial: Vec<Vec<f32>> = frames
            .frames
            .iter()
            .map(spatial::extract_frame_features)
            .collect();

        // Optical flow lives BETWEEN frames, so a single frame's features
        // cannot carry it -- `extract_frame_features` leaves that section
        // zero. Fill it here, where the pairs exist. Left unfilled, 32 of the
        // 192 spatial dims were a constant zero in every video ever encoded,
        // and motion.rs had no caller at all.
        let (motion_mean, motion_std, motion_max) =
            fill_flow_section(&frames.frames, &mut per_frame_spatial);

        let shot_boundaries = shot::detect_shots(&per_frame_spatial);
        let spatial_agg = spatial::aggregate_spatial(&per_frame_spatial);
        let temporal = temporal::extract_temporal_features(&per_frame_spatial, &shot_boundaries);

        let mut vector = spatial_agg.vector.clone();
        vector.extend_from_slice(&temporal.vector);
        debug_assert_eq!(vector.len(), VIDEO_FEATURE_DIM);

        let dominant_colors = read_dominant_colors(&spatial_agg.vector);
        let shot_count = shot_boundaries.len() + 1;
        let feature_tags = build_tags(
            motion_mean,
            spatial_agg.mean_brightness,
            spatial_agg.mean_contrast,
            shot_count,
            &dominant_colors,
        );

        Ok(VideoFeatures {
            vector,
            spatial: spatial_agg,
            temporal,
            duration_secs: frames.duration_secs,
            frame_count: frames.frames.len(),
            analysis_fps: frames.analysis_fps,
            shot_boundaries,
            shot_count,
            motion_mean,
            motion_std,
            motion_max,
            dominant_colors,
            feature_tags,
        })
    }

    /// Access the underlying codebook (for tests).
    pub fn codebook(&self) -> &Codebook {
        &self.codebook
    }
}

impl Default for VideoPipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ── Pipeline helpers ───────────────────────────────────────

/// Fill the optical-flow section of each frame's spatial features from block
/// matching against the previous frame. Returns (mean, std, max) of per-step
/// motion magnitude in pixels.
///
/// Frame 0 has no predecessor; it inherits frame 1's flow rather than staying
/// zero, so the sequence does not open with a spurious jump that the temporal
/// motion trajectory would read as a burst of movement.
fn fill_flow_section(frames: &[FrameInfo], per_frame: &mut [Vec<f32>]) -> (f32, f32, f32) {
    if frames.len() < 2 || per_frame.len() < 2 {
        return (0.0, 0.0, 0.0);
    }

    let mut all_vectors: Vec<Vec<MotionVector>> = Vec::with_capacity(frames.len() - 1);
    for i in 1..frames.len().min(per_frame.len()) {
        let vectors = motion::block_motion(&frames[i - 1], &frames[i]);
        let hist = motion::flow_histogram(&vectors);
        let dst = &mut per_frame[i];
        for (b, &h) in hist.iter().enumerate() {
            if FLOW_OFFSET + b < dst.len() {
                dst[FLOW_OFFSET + b] = h;
            }
        }
        all_vectors.push(vectors);
    }

    // Frame 0 inherits frame 1's flow.
    let (head, tail) = per_frame.split_at_mut(1);
    let end = (FLOW_OFFSET + FLOW_BINS)
        .min(head[0].len())
        .min(tail[0].len());
    if end > FLOW_OFFSET {
        head[0][FLOW_OFFSET..end].copy_from_slice(&tail[0][FLOW_OFFSET..end]);
    }

    motion::motion_stats(&all_vectors)
}

/// Read the dominant-colour section back out of an aggregated spatial vector.
fn read_dominant_colors(spatial_vector: &[f32]) -> Vec<DominantColor> {
    let mut out = Vec::new();
    for k in 0..4 {
        let base = DOMINANT_OFFSET + k * 3;
        if base + 2 >= spatial_vector.len() {
            break;
        }
        let hue = spatial_vector[base] * 360.0;
        let saturation = spatial_vector[base + 1];
        let value = spatial_vector[base + 2];
        if saturation == 0.0 && value == 0.0 {
            continue; // padding, not a real centroid
        }
        out.push(DominantColor {
            hue,
            saturation,
            value,
            name: color_name(hue, saturation, value),
        });
    }
    out
}

/// Categorical descriptors, in the shape `src/ear` uses for audio: coarse
/// words that cluster, not high-cardinality numbers that fragment.
fn build_tags(
    motion_mean: f32,
    mean_brightness: f32,
    mean_contrast: f32,
    shot_count: usize,
    dominant: &[DominantColor],
) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();

    tags.push(
        if motion_mean < 0.25 {
            "static"
        } else if motion_mean < 1.5 {
            "slow-motion"
        } else if motion_mean < 4.0 {
            "moving"
        } else {
            "fast-motion"
        }
        .into(),
    );

    if mean_brightness > 170.0 {
        tags.push("bright".into());
    } else if mean_brightness < 70.0 {
        tags.push("dark".into());
    }

    if mean_contrast > 50.0 {
        tags.push("high-contrast".into());
    } else if mean_contrast < 12.0 {
        tags.push("flat".into());
    }

    if shot_count >= 8 {
        tags.push("many-cuts".into());
    } else if shot_count > 1 {
        tags.push("cut".into());
    } else {
        tags.push("single-shot".into());
    }

    if let Some(first) = dominant.first() {
        tags.push(first.name.into());
    }

    tags
}

// ── Test fixtures ──────────────────────────────────────────

/// Synthetic frame sequences, so the perception path can be exercised without
/// a video fixture on disk or ffmpeg on PATH.
#[cfg(test)]
pub(crate) mod testframes {
    use super::{FrameInfo, VideoFrames};

    pub fn frame_from<F: Fn(u32, u32) -> (u8, u8, u8)>(
        w: u32,
        h: u32,
        index: usize,
        f: F,
    ) -> FrameInfo {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = f(x, y);
                rgb.extend_from_slice(&[r, g, b]);
            }
        }
        FrameInfo { rgb, width: w, height: h, index }
    }

    pub fn wrap(frames: Vec<FrameInfo>, fps: f32) -> VideoFrames {
        let (w, h) = (frames[0].width, frames[0].height);
        VideoFrames {
            duration_secs: frames.len() as f32 / fps,
            frames,
            width: w,
            height: h,
            source_fps: 30.0,
            analysis_fps: fps,
        }
    }

    /// A clip that never changes.
    pub fn solid_clip(n: usize, rgb: (u8, u8, u8)) -> VideoFrames {
        let frames = (0..n)
            .map(|i| frame_from(96, 64, i, |_, _| rgb))
            .collect();
        wrap(frames, 2.0)
    }

    /// A textured clip that never changes -- something block matching can lock
    /// onto, unlike a flat colour field.
    pub fn static_textured_clip(n: usize) -> VideoFrames {
        let frames = (0..n).map(|i| stripe_frame(i, 0)).collect();
        wrap(frames, 2.0)
    }

    /// A textured clip panning right by `step` pixels per frame.
    pub fn panning_clip(n: usize, step: i32) -> VideoFrames {
        let frames = (0..n)
            .map(|i| stripe_frame(i, i as i32 * step))
            .collect();
        wrap(frames, 2.0)
    }

    /// `n` frames of colour `a`, then `n` of colour `b`: one hard cut.
    pub fn hard_cut_clip(n: usize, a: (u8, u8, u8), b: (u8, u8, u8)) -> VideoFrames {
        let mut frames: Vec<FrameInfo> = (0..n)
            .map(|i| frame_from(96, 64, i, |_, _| a))
            .collect();
        frames.extend((0..n).map(|i| frame_from(96, 64, n + i, |_, _| b)));
        wrap(frames, 2.0)
    }

    fn stripe_frame(index: usize, shift: i32) -> FrameInfo {
        frame_from(96, 64, index, move |x, y| {
            let sx = x as i32 - shift;
            let v = if sx.rem_euclid(23) < 11 { 240u8 } else { 20u8 };
            let u = if (y as i32).rem_euclid(31) < 15 { 30u8 } else { 0u8 };
            (v.saturating_add(u), v, v)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testframes::*;
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }

    #[test]
    fn the_feature_vector_is_spatial_plus_temporal() {
        assert_eq!(VIDEO_FEATURE_DIM, SPATIAL_FEATURE_DIM + TEMPORAL_FEATURE_DIM);
        assert_eq!(VIDEO_FEATURE_DIM, 320);
        assert_eq!(DOMINANT_OFFSET + 16, SPATIAL_FEATURE_DIM);
    }

    #[test]
    fn an_empty_clip_is_rejected_not_panicked_on() {
        let empty = VideoFrames {
            frames: vec![],
            width: 0,
            height: 0,
            duration_secs: 0.0,
            source_fps: 0.0,
            analysis_fps: 2.0,
        };
        assert!(matches!(
            VideoPipeline::new().analyze(&empty),
            Err(EyeError::EmptyVideo)
        ));
    }

    #[test]
    fn a_one_frame_clip_is_too_short_for_temporal_perception() {
        let one = solid_clip(1, (10, 20, 30));
        assert!(matches!(
            VideoPipeline::new().analyze(&one),
            Err(EyeError::TooShort)
        ));
    }

    #[test]
    fn the_pipeline_projects_into_the_shared_hypervector_space() {
        let clip = static_textured_clip(6);
        let (mem, vf) = VideoPipeline::new()
            .encode_frames(&clip, "static")
            .expect("static clip encodes");

        assert_eq!(vf.vector.len(), VIDEO_FEATURE_DIM);
        assert_eq!(mem.vector.len(), HYPERVECTOR_DIM);
        assert!(mem.vector.iter().all(|v| v.is_finite()));
        assert!(mem.content.starts_with("video:"));
        // ADR-0008 wave parameters: visual memories sit at their own phase.
        assert!((mem.phase - PI / 2.0).abs() < 1e-6);
        assert!((mem.frequency - 0.03).abs() < 1e-9);
    }

    #[test]
    fn a_hard_cut_between_two_solid_colors_produces_exactly_one_boundary() {
        let clip = hard_cut_clip(5, (220, 30, 30), (30, 40, 220));
        let vf = VideoPipeline::new().analyze(&clip).expect("cut clip analyses");
        assert_eq!(
            vf.shot_boundaries.len(),
            1,
            "one cut, got boundaries {:?}",
            vf.shot_boundaries
        );
        assert_eq!(vf.shot_boundaries[0], 5, "the cut is at frame 5");
        assert_eq!(vf.shot_count, 2);
        assert!(vf.feature_tags.iter().any(|t| t == "cut"));
    }

    #[test]
    fn an_unchanging_clip_has_no_cuts_and_no_motion() {
        let clip = static_textured_clip(8);
        let vf = VideoPipeline::new().analyze(&clip).expect("analyses");
        assert!(vf.shot_boundaries.is_empty(), "nothing changes, nothing cuts");
        assert_eq!(vf.shot_count, 1);
        assert!(
            vf.motion_mean < 0.01,
            "a static clip must report ~zero motion, got {}",
            vf.motion_mean
        );
        assert!(vf.feature_tags.iter().any(|t| t == "static"));
        assert!(vf.feature_tags.iter().any(|t| t == "single-shot"));
    }

    #[test]
    fn a_panning_clip_reports_motion_in_the_pan_magnitude() {
        let clip = panning_clip(8, 4);
        let vf = VideoPipeline::new().analyze(&clip).expect("analyses");
        assert!(
            (vf.motion_mean - 4.0).abs() < 1.0,
            "a 4 px/frame pan should measure near 4 px, got {}",
            vf.motion_mean
        );
        assert!(!vf.feature_tags.iter().any(|t| t == "static"));
    }

    #[test]
    fn the_flow_band_separates_a_still_clip_from_a_moving_one() {
        // This is the band that was a block of 32 hardcoded zeros before the
        // pipeline learned to fill it: motion.rs had no caller at all.
        let still = VideoPipeline::new()
            .analyze(&static_textured_clip(8))
            .unwrap();
        let moving = VideoPipeline::new().analyze(&panning_clip(8, 4)).unwrap();

        let band = |v: &VideoFeatures| v.spatial.vector[FLOW_OFFSET..FLOW_OFFSET + FLOW_BINS].to_vec();
        let (sb, mb) = (band(&still), band(&moving));

        assert!(
            sb[0] > 0.95,
            "a still clip puts its flow mass in the zero-motion bin: {sb:?}"
        );
        assert!(
            mb[0] < 0.5,
            "a panning clip must not sit in the zero-motion bin: {mb:?}"
        );
        let l1: f32 = sb.iter().zip(&mb).map(|(a, b)| (a - b).abs()).sum();
        assert!(l1 > 1.0, "the two flow bands differ by only {l1}");
    }

    #[test]
    fn clips_of_different_content_get_different_hypervectors() {
        let pipeline = VideoPipeline::new();
        let (red, _) = pipeline
            .encode_frames(&solid_clip(8, (220, 30, 30)), "red")
            .unwrap();
        let (blue, _) = pipeline
            .encode_frames(&solid_clip(8, (30, 40, 220)), "blue")
            .unwrap();
        let (pan, _) = pipeline.encode_frames(&panning_clip(8, 4), "pan").unwrap();

        for (name, a, b) in [
            ("red vs blue", &red, &blue),
            ("red vs pan", &red, &pan),
            ("blue vs pan", &blue, &pan),
        ] {
            let sim = cosine(&a.vector, &b.vector);
            assert!(
                sim < 0.99,
                "{name} should not collapse to the same hypervector (cos {sim})"
            );
        }
    }

    #[test]
    fn the_same_clip_encodes_to_the_same_hypervector() {
        let pipeline = VideoPipeline::new();
        let (a, _) = pipeline.encode_frames(&panning_clip(8, 4), "a").unwrap();
        let (b, _) = pipeline.encode_frames(&panning_clip(8, 4), "b").unwrap();
        assert!(
            cosine(&a.vector, &b.vector) > 0.9999,
            "perception must be deterministic"
        );
    }

    #[test]
    fn a_red_clip_is_named_red() {
        let vf = VideoPipeline::new()
            .analyze(&solid_clip(6, (220, 20, 20)))
            .unwrap();
        assert!(
            vf.dominant_colors.iter().any(|c| c.name == "red"),
            "dominant colours {:?}",
            vf.dominant_colors
        );
        assert!(vf.feature_tags.iter().any(|t| t == "red"));
    }

    #[test]
    fn color_naming_checks_value_and_saturation_before_hue() {
        // Hue is meaningless when there is nothing to be hued.
        assert_eq!(color_name(30.0, 1.0, 0.01), "black");
        assert_eq!(color_name(30.0, 0.01, 0.5), "grey");
        assert_eq!(color_name(30.0, 0.01, 0.95), "white");
        assert_eq!(color_name(0.0, 1.0, 1.0), "red");
        assert_eq!(color_name(240.0, 1.0, 1.0), "blue");
        assert_eq!(color_name(355.0, 1.0, 1.0), "red", "hue wraps back to red");
    }

    #[test]
    fn brightness_tags_track_the_clip() {
        let dark = VideoPipeline::new().analyze(&solid_clip(6, (5, 5, 5))).unwrap();
        assert!(dark.feature_tags.iter().any(|t| t == "dark"), "{:?}", dark.feature_tags);

        let bright = VideoPipeline::new()
            .analyze(&solid_clip(6, (250, 250, 250)))
            .unwrap();
        assert!(
            bright.feature_tags.iter().any(|t| t == "bright"),
            "{:?}",
            bright.feature_tags
        );
    }

    #[test]
    fn the_video_codebook_is_its_own_subspace() {
        // ADR-0008 principle 1: visual vectors are orthogonal to text (42)
        // and audio (0xEA5).
        assert_eq!(VIDEO_CODEBOOK_SEED, 0x3E5E);
        assert_ne!(VIDEO_CODEBOOK_SEED, crate::ear::AUDIO_CODEBOOK_SEED);
        assert_ne!(VIDEO_CODEBOOK_SEED, 42);
    }
}
