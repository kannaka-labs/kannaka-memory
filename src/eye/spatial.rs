//! Per-frame spatial feature extraction (192 dims) and aggregation.
//!
//! Features: HSV histogram (48), spatial frequency (32), edge orientation (36),
//! region statistics (20), optical flow magnitude (32), contrast/brightness (8),
//! dominant colors (16).

use super::color::{hsv_histogram, dominant_colors};
use super::decode::FrameInfo;
use super::{EDGE_BINS, FLOW_BINS, FREQ_BANDS, REGION_GRID, SPATIAL_FEATURE_DIM};

/// Aggregated spatial features across all frames.
#[derive(Debug, Clone)]
pub struct SpatialFeatures {
    /// 192-dim vector (mean of per-frame features).
    pub vector: Vec<f32>,
    /// Mean brightness (0-255).
    pub mean_brightness: f32,
    /// Mean contrast.
    pub mean_contrast: f32,
}

/// Extract per-frame spatial features (192 dims).
pub fn extract_frame_features(frame: &FrameInfo) -> Vec<f32> {
    let mut features = Vec::with_capacity(SPATIAL_FEATURE_DIM);

    // 1. HSV histogram (48 dims: 16 bins × 3 channels)
    let hsv_hist = hsv_histogram(&frame.rgb, frame.width, frame.height);
    features.extend_from_slice(&hsv_hist);

    // 2. Spatial frequency via simple block variance (32 dims)
    let freq = spatial_frequency(frame);
    features.extend_from_slice(&freq);

    // 3. Edge orientation histogram (36 dims)
    let edges = edge_orientation(frame);
    features.extend_from_slice(&edges);

    // 4. Region statistics (20 dims: 4×5 grid mean luminance)
    let regions = region_statistics(frame);
    features.extend_from_slice(&regions);

    // 5. Optical flow (32 dims) — left zero here and filled by the pipeline.
    // Flow is a property of a frame PAIR, which a single frame cannot see;
    // `VideoPipeline::analyze` writes this section at `FLOW_OFFSET` once it
    // has the predecessor. A frame analysed on its own keeps zeros.
    features.extend(std::iter::repeat(0.0f32).take(FLOW_BINS));

    // 6. Contrast/brightness (8 dims)
    let cb = contrast_brightness(frame);
    features.extend_from_slice(&cb);

    // 7. Dominant colors (16 dims: 4 colors × HSV + weight)
    let dc = dominant_colors(&frame.rgb, frame.width, frame.height, 4);
    // dc is 12 dims (4 × 3 HSV), pad to 16
    features.extend_from_slice(&dc);
    let remaining = 16 - dc.len().min(16);
    features.extend(std::iter::repeat(0.0f32).take(remaining));

    // Ensure exact dimension
    features.truncate(SPATIAL_FEATURE_DIM);
    while features.len() < SPATIAL_FEATURE_DIM {
        features.push(0.0);
    }

    features
}

/// Spatial frequency approximation using block variance of luminance.
/// Divides frame into blocks and computes variance per block, then
/// bins into frequency bands.
fn spatial_frequency(frame: &FrameInfo) -> Vec<f32> {
    let block_size = 8u32;
    let bx = (frame.width / block_size).max(1);
    let by = (frame.height / block_size).max(1);

    let mut variances: Vec<f32> = Vec::new();

    for by_idx in 0..by {
        for bx_idx in 0..bx {
            let ox = bx_idx * block_size;
            let oy = by_idx * block_size;

            let mut sum = 0.0f32;
            let mut sum_sq = 0.0f32;
            let mut count = 0.0f32;

            for py in 0..block_size.min(frame.height - oy) {
                for px in 0..block_size.min(frame.width - ox) {
                    let l = frame.luminance(ox + px, oy + py);
                    sum += l;
                    sum_sq += l * l;
                    count += 1.0;
                }
            }

            if count > 1.0 {
                let mean = sum / count;
                let var = (sum_sq / count) - mean * mean;
                variances.push(var.max(0.0));
            }
        }
    }

    // Bin variances into FREQ_BANDS
    bin_values(&variances, FREQ_BANDS)
}

/// Sobel edge orientation histogram (36 bins × 10°).
fn edge_orientation(frame: &FrameInfo) -> Vec<f32> {
    let mut hist = vec![0.0f32; EDGE_BINS];
    let w = frame.width;
    let h = frame.height;

    if w < 3 || h < 3 {
        return hist;
    }

    // Sample every 2nd pixel for speed
    let step = 2u32;
    let mut count = 0.0f32;

    for y in (1..h - 1).step_by(step as usize) {
        for x in (1..w - 1).step_by(step as usize) {
            // Sobel gradients
            let gx = -frame.luminance(x - 1, y - 1) - 2.0 * frame.luminance(x - 1, y) - frame.luminance(x - 1, y + 1)
                   + frame.luminance(x + 1, y - 1) + 2.0 * frame.luminance(x + 1, y) + frame.luminance(x + 1, y + 1);

            let gy = -frame.luminance(x - 1, y - 1) - 2.0 * frame.luminance(x, y - 1) - frame.luminance(x + 1, y - 1)
                   + frame.luminance(x - 1, y + 1) + 2.0 * frame.luminance(x, y + 1) + frame.luminance(x + 1, y + 1);

            let mag = (gx * gx + gy * gy).sqrt();
            if mag > 10.0 {
                // Angle in [0, π) → bin
                let angle = gy.atan2(gx); // [-π, π]
                let angle_pos = if angle < 0.0 { angle + std::f32::consts::PI } else { angle };
                let bin = ((angle_pos / std::f32::consts::PI * EDGE_BINS as f32) as usize).min(EDGE_BINS - 1);
                hist[bin] += mag;
                count += mag;
            }
        }
    }

    // Normalize
    if count > 0.0 {
        for h in hist.iter_mut() {
            *h /= count;
        }
    }

    hist
}

/// Region statistics: mean luminance in a REGION_GRID spatial grid.
fn region_statistics(frame: &FrameInfo) -> Vec<f32> {
    let (rows, cols) = REGION_GRID;
    let rh = (frame.height / rows as u32).max(1);
    let rw = (frame.width / cols as u32).max(1);

    let mut stats = Vec::with_capacity(rows * cols);

    for r in 0..rows {
        for c in 0..cols {
            let ox = c as u32 * rw;
            let oy = r as u32 * rh;
            let mut sum = 0.0f32;
            let mut count = 0.0f32;

            for py in 0..rh.min(frame.height - oy) {
                for px in 0..rw.min(frame.width - ox) {
                    sum += frame.luminance(ox + px, oy + py);
                    count += 1.0;
                }
            }

            stats.push(if count > 0.0 { sum / count / 255.0 } else { 0.0 });
        }
    }

    stats
}

/// Contrast and brightness statistics (8 dims).
fn contrast_brightness(frame: &FrameInfo) -> Vec<f32> {
    let npx = frame.pixel_count();
    if npx == 0 {
        return vec![0.0; 8];
    }

    let mut min_l = f32::MAX;
    let mut max_l = f32::MIN;
    let mut sum = 0.0f32;
    let mut sum_sq = 0.0f32;

    // Sample for speed
    let step = (npx / 5000).max(1);
    let mut count = 0.0f32;

    for i in (0..npx).step_by(step) {
        let x = (i % frame.width as usize) as u32;
        let y = (i / frame.width as usize) as u32;
        let l = frame.luminance(x, y);
        sum += l;
        sum_sq += l * l;
        if l < min_l { min_l = l; }
        if l > max_l { max_l = l; }
        count += 1.0;
    }

    let mean = sum / count;
    let variance = (sum_sq / count) - mean * mean;
    let std = variance.max(0.0).sqrt();

    // Local contrast: std of luminance in center region
    let cx = frame.width / 4;
    let cy = frame.height / 4;
    let cw = frame.width / 2;
    let ch = frame.height / 2;
    let mut center_sum = 0.0f32;
    let mut center_sq = 0.0f32;
    let mut center_count = 0.0f32;
    let center_step = ((cw * ch) as usize / 1000).max(1);

    for i in (0..(cw * ch) as usize).step_by(center_step) {
        let lx = cx + (i as u32 % cw);
        let ly = cy + (i as u32 / cw);
        if lx < frame.width && ly < frame.height {
            let l = frame.luminance(lx, ly);
            center_sum += l;
            center_sq += l * l;
            center_count += 1.0;
        }
    }

    let center_mean = if center_count > 0.0 { center_sum / center_count } else { mean };
    let center_var = if center_count > 1.0 { (center_sq / center_count) - center_mean * center_mean } else { 0.0 };
    let local_contrast = center_var.max(0.0).sqrt();

    vec![
        mean / 255.0,           // normalized mean brightness
        std / 128.0,            // normalized std
        min_l / 255.0,          // min
        max_l / 255.0,          // max
        (max_l - min_l) / 255.0, // range
        local_contrast / 128.0, // local contrast
        center_mean / 255.0,    // center brightness
        variance / (128.0 * 128.0), // normalized variance
    ]
}

/// Aggregate per-frame spatial features into a single 192-dim vector.
/// Uses mean across all frames.
pub fn aggregate_spatial(per_frame: &[Vec<f32>]) -> SpatialFeatures {
    let dim = per_frame[0].len();
    let n = per_frame.len() as f32;
    let mut mean_vec = vec![0.0f32; dim];

    for frame_features in per_frame {
        for (i, &v) in frame_features.iter().enumerate() {
            mean_vec[i] += v;
        }
    }
    for v in mean_vec.iter_mut() {
        *v /= n;
    }

    // Extract brightness/contrast from the aggregated contrast_brightness
    // section. `CONTRAST_OFFSET` names the boundary so this stops being a
    // hand-summed literal that silently goes stale if a section resizes.
    let brightness_idx = super::CONTRAST_OFFSET;
    let mean_brightness = if dim > brightness_idx {
        mean_vec[brightness_idx] * 255.0
    } else {
        128.0
    };
    let mean_contrast = if dim > brightness_idx + 5 {
        mean_vec[brightness_idx + 5] * 128.0
    } else {
        0.0
    };

    SpatialFeatures {
        vector: mean_vec,
        mean_brightness,
        mean_contrast,
    }
}

/// Bin a slice of values into `n_bins` using histogram.
fn bin_values(values: &[f32], n_bins: usize) -> Vec<f32> {
    if values.is_empty() {
        return vec![0.0; n_bins];
    }

    let max = values.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    let mut hist = vec![0.0f32; n_bins];

    for &v in values {
        let bin = ((v / max * n_bins as f32) as usize).min(n_bins - 1);
        hist[bin] += 1.0;
    }

    // Normalize
    let total: f32 = hist.iter().sum();
    if total > 0.0 {
        for h in hist.iter_mut() {
            *h /= total;
        }
    }

    hist
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eye::{CONTRAST_OFFSET, DOMINANT_OFFSET, FLOW_OFFSET, REGION_OFFSET};

    fn frame_from<F: Fn(u32, u32) -> (u8, u8, u8)>(w: u32, h: u32, f: F) -> FrameInfo {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = f(x, y);
                rgb.extend_from_slice(&[r, g, b]);
            }
        }
        FrameInfo { rgb, width: w, height: h, index: 0 }
    }

    fn solid(w: u32, h: u32, r: u8, g: u8, b: u8) -> FrameInfo {
        frame_from(w, h, move |_, _| (r, g, b))
    }

    #[test]
    fn frame_features_are_exactly_the_declared_dimension() {
        let f = extract_frame_features(&solid(80, 60, 12, 200, 90));
        assert_eq!(f.len(), SPATIAL_FEATURE_DIM);
        assert!(f.iter().all(|v| v.is_finite()), "no NaN or inf in features");
    }

    #[test]
    fn the_feature_sections_tile_the_vector_without_overlap() {
        // The offsets must still add up to the declared dimension; a section
        // that changes size without its offset moving would silently overwrite
        // its neighbour.
        assert_eq!(DOMINANT_OFFSET + 16, SPATIAL_FEATURE_DIM);
        assert!(FLOW_OFFSET < CONTRAST_OFFSET);
        assert!(REGION_OFFSET < FLOW_OFFSET);
    }

    #[test]
    fn a_single_frame_carries_no_optical_flow() {
        // Flow needs a frame pair; on its own a frame leaves that band zero
        // and the pipeline fills it in. This is the documented contract the
        // pipeline relies on.
        let f = extract_frame_features(&solid(80, 60, 200, 40, 40));
        assert!(
            f[FLOW_OFFSET..FLOW_OFFSET + FLOW_BINS].iter().all(|&v| v == 0.0),
            "the flow band of a lone frame must be zero"
        );
    }

    #[test]
    fn a_black_frame_and_a_white_frame_differ_in_the_brightness_dim() {
        let black = extract_frame_features(&solid(80, 60, 0, 0, 0));
        let white = extract_frame_features(&solid(80, 60, 255, 255, 255));
        assert!(black[CONTRAST_OFFSET] < 0.05, "black is dark");
        assert!(white[CONTRAST_OFFSET] > 0.95, "white is bright");
    }

    #[test]
    fn region_statistics_locate_a_bright_quadrant() {
        // REGION_GRID is (rows, cols) = (4, 5). Light only the top-left cell.
        let (rows, cols) = REGION_GRID;
        let (w, h) = (80u32, 60u32);
        let rw = w / cols as u32;
        let rh = h / rows as u32;
        let f = extract_frame_features(&frame_from(w, h, move |x, y| {
            if x < rw && y < rh {
                (255, 255, 255)
            } else {
                (0, 0, 0)
            }
        }));

        let regions = &f[REGION_OFFSET..REGION_OFFSET + rows * cols];
        assert!(regions[0] > 0.9, "top-left region is lit: {}", regions[0]);
        for (i, &v) in regions.iter().enumerate().skip(1) {
            assert!(v < 0.1, "region {i} should be dark, got {v}");
        }
    }

    #[test]
    fn aggregate_of_identical_frames_reproduces_the_frame() {
        let one = extract_frame_features(&solid(80, 60, 30, 120, 210));
        let agg = aggregate_spatial(&vec![one.clone(); 5]);
        assert_eq!(agg.vector.len(), SPATIAL_FEATURE_DIM);
        for (i, (&a, &b)) in agg.vector.iter().zip(&one).enumerate() {
            assert!((a - b).abs() < 1e-5, "dim {i}: {a} vs {b}");
        }
    }

    #[test]
    fn aggregate_reports_brightness_on_the_0_to_255_scale() {
        let white = extract_frame_features(&solid(80, 60, 255, 255, 255));
        let agg = aggregate_spatial(&[white]);
        assert!(
            agg.mean_brightness > 250.0,
            "a white clip is near 255, got {}",
            agg.mean_brightness
        );

        let black = extract_frame_features(&solid(80, 60, 0, 0, 0));
        let agg = aggregate_spatial(&[black]);
        assert!(
            agg.mean_brightness < 5.0,
            "a black clip is near 0, got {}",
            agg.mean_brightness
        );
    }

    #[test]
    fn an_edgy_frame_carries_more_edge_energy_than_a_flat_one() {
        let flat = extract_frame_features(&solid(80, 60, 128, 128, 128));
        let stripes = extract_frame_features(&frame_from(80, 60, |x, _| {
            if x % 8 < 4 { (255, 255, 255) } else { (0, 0, 0) }
        }));
        let edge_sum = |v: &Vec<f32>| -> f32 {
            v[crate::eye::EDGE_OFFSET..crate::eye::EDGE_OFFSET + EDGE_BINS]
                .iter()
                .sum()
        };
        assert!(edge_sum(&flat) < 0.01, "a flat frame has no edges");
        assert!(edge_sum(&stripes) > 0.9, "stripes normalize to ~1 of edge mass");
    }
}
