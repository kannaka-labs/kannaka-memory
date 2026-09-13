//! Optical flow estimation — simple block-matching approach.
//!
//! No OpenCV dependency. We compute approximate motion vectors between
//! consecutive frames using a block-matching algorithm.

use super::decode::FrameInfo;
use super::FLOW_BINS;

/// Motion vector (dx, dy) in pixels.
#[derive(Debug, Clone, Copy)]
pub struct MotionVector {
    pub dx: f32,
    pub dy: f32,
}

impl MotionVector {
    pub fn magnitude(&self) -> f32 {
        (self.dx * self.dx + self.dy * self.dy).sqrt()
    }

    pub fn angle(&self) -> f32 {
        self.dy.atan2(self.dx)
    }
}

/// Precompute a frame's luminance plane once, so block matching indexes a
/// flat `f32` slice instead of recomputing BT.601 per candidate offset.
fn luminance_plane(frame: &FrameInfo) -> Vec<f32> {
    let n = frame.pixel_count();
    let mut plane = Vec::with_capacity(n);
    for i in 0..n {
        let base = i * 3;
        plane.push(
            0.299 * frame.rgb[base] as f32
                + 0.587 * frame.rgb[base + 1] as f32
                + 0.114 * frame.rgb[base + 2] as f32,
        );
    }
    plane
}

/// Compute global motion between two frames using block matching.
///
/// Divides frame into blocks and finds best match in search window.
/// Returns per-block motion vectors.
///
/// The SAD is full-density: every pixel of the block, every candidate offset.
/// Sampling the block on a 2-pixel lattice is ~4x faster but makes odd and
/// even displacements alias, so a 4 px pan comes back as a mix of 3 and 4.
/// Precomputing each frame's luminance plane recovers most of that speed
/// without costing accuracy, so the density stays.
pub fn block_motion(prev: &FrameInfo, curr: &FrameInfo) -> Vec<MotionVector> {
    let block_size: u32 = 16;
    let search_range: i32 = 8;

    let bx = prev.width / block_size;
    let by = prev.height / block_size;
    let mut vectors = Vec::with_capacity((bx * by) as usize);

    if bx == 0 || by == 0 {
        return vectors;
    }

    let prev_lum = luminance_plane(prev);
    let curr_lum = luminance_plane(curr);
    let pw = prev.width as usize;
    let cw = curr.width as usize;

    for by_idx in 0..by {
        for bx_idx in 0..bx {
            let ox = bx_idx * block_size;
            let oy = by_idx * block_size;

            let mut best_dx: i32 = 0;
            let mut best_dy: i32 = 0;
            let mut best_sad = f32::MAX;
            let mut best_disp: i32 = i32::MAX;

            // Search window
            for dy in -search_range..=search_range {
                for dx in -search_range..=search_range {
                    let sx = ox as i32 + dx;
                    let sy = oy as i32 + dy;

                    // Bounds check
                    if sx < 0
                        || sy < 0
                        || (sx + block_size as i32) > curr.width as i32
                        || (sy + block_size as i32) > curr.height as i32
                    {
                        continue;
                    }

                    // Sum of absolute differences over the whole block.
                    let mut sad = 0.0f32;
                    for py in 0..block_size {
                        let prow = (oy + py) as usize * pw + ox as usize;
                        let crow = (sy + py as i32) as usize * cw + sx as usize;
                        for px in 0..block_size as usize {
                            sad += (prev_lum[prow + px] - curr_lum[crow + px]).abs();
                        }
                    }

                    // Break SAD ties toward the smallest displacement. Without
                    // this, a flat or uniform block matches every candidate
                    // offset equally and the search reports whichever the loop
                    // reached first (-8,-8) — a static frame pair would come
                    // back at maximum motion. Smallest-displacement is also the
                    // right prior for real footage: given equal evidence, the
                    // block did not move.
                    let disp = dx * dx + dy * dy;
                    if sad < best_sad || (sad == best_sad && disp < best_disp) {
                        best_sad = sad;
                        best_disp = disp;
                        best_dx = dx;
                        best_dy = dy;
                    }
                }
            }

            vectors.push(MotionVector {
                dx: best_dx as f32,
                dy: best_dy as f32,
            });
        }
    }

    vectors
}

/// Compute optical flow magnitude histogram from motion vectors.
/// Returns `FLOW_BINS` bins of radially-binned motion energy.
pub fn flow_histogram(vectors: &[MotionVector]) -> Vec<f32> {
    if vectors.is_empty() {
        return vec![0.0; FLOW_BINS];
    }

    let max_mag = vectors.iter().map(|v| v.magnitude()).fold(0.0f32, f32::max).max(1.0);
    let mut hist = vec![0.0f32; FLOW_BINS];

    for v in vectors {
        let mag = v.magnitude();
        let bin = ((mag / max_mag * FLOW_BINS as f32) as usize).min(FLOW_BINS - 1);
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

/// Compute aggregate motion statistics from a sequence of frame-pair motion vectors.
pub fn motion_stats(all_vectors: &[Vec<MotionVector>]) -> (f32, f32, f32) {
    // mean, std, max of per-frame average magnitude
    let per_frame_mag: Vec<f32> = all_vectors
        .iter()
        .map(|vecs| {
            if vecs.is_empty() {
                0.0
            } else {
                let sum: f32 = vecs.iter().map(|v| v.magnitude()).sum();
                sum / vecs.len() as f32
            }
        })
        .collect();

    let n = per_frame_mag.len() as f32;
    if n < 1.0 {
        return (0.0, 0.0, 0.0);
    }

    let mean = per_frame_mag.iter().sum::<f32>() / n;
    let variance = per_frame_mag.iter().map(|m| (m - mean).powi(2)).sum::<f32>() / n;
    let std = variance.sqrt();
    let max = per_frame_mag.iter().cloned().fold(0.0f32, f32::max);

    (mean, std, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eye::decode::FrameInfo;

    /// Build a frame whose pixel colour is decided by a closure over (x, y).
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

    /// A vertical-stripe texture that block matching can actually lock onto,
    /// shifted right by `shift` pixels.
    fn stripes(w: u32, h: u32, shift: i32) -> FrameInfo {
        frame_from(w, h, |x, y| {
            let sx = x as i32 - shift;
            // Two interleaved periods so the pattern is not ambiguous within
            // the +/-8 px search window.
            let v = if (sx.rem_euclid(23)) < 11 { 240u8 } else { 20u8 };
            let u = if (y as i32).rem_euclid(31) < 15 { 30u8 } else { 0u8 };
            (v.saturating_add(u), v, v)
        })
    }

    #[test]
    fn a_static_frame_pair_reports_zero_motion() {
        let a = stripes(96, 64, 0);
        let b = stripes(96, 64, 0);
        let vectors = block_motion(&a, &b);
        assert!(!vectors.is_empty(), "96x64 must yield blocks");
        for v in &vectors {
            assert_eq!(
                (v.dx, v.dy),
                (0.0, 0.0),
                "an identical frame pair must not move"
            );
        }
    }

    #[test]
    fn a_solid_color_pair_reports_zero_motion() {
        // Uniform blocks match every candidate offset equally; the search must
        // break that tie toward no displacement rather than toward the corner
        // of the search window.
        let a = frame_from(96, 64, |_, _| (90, 120, 200));
        let b = frame_from(96, 64, |_, _| (90, 120, 200));
        for v in block_motion(&a, &b) {
            assert_eq!((v.dx, v.dy), (0.0, 0.0));
        }
    }

    #[test]
    fn a_horizontal_pan_is_detected_in_the_pan_direction() {
        // Content moves +4 px to the right between frames. Block matching
        // searches for where the PREVIOUS block went, so the match is found
        // at dx = +4.
        let a = stripes(96, 64, 0);
        let b = stripes(96, 64, 4);
        let vectors = block_motion(&a, &b);

        // The rightmost block column ends flush with the frame edge, so it has
        // no room to search rightward and cannot see the pan. Judge the rest.
        let found = vectors.iter().map(|v| (v.dx, v.dy)).collect::<Vec<_>>();
        let agree = vectors.iter().filter(|v| v.dx == 4.0 && v.dy == 0.0).count();
        assert!(
            agree * 2 > vectors.len(),
            "most blocks should report dx=+4, dy=0; got {found:?}"
        );
        assert!(
            vectors.iter().all(|v| v.dx >= 0.0),
            "nothing moved leftward; got {found:?}"
        );
    }

    #[test]
    fn a_vertical_pan_is_detected_on_the_vertical_axis() {
        let a = frame_from(96, 64, |x, y| {
            let v = if (y as i32).rem_euclid(23) < 11 { 240u8 } else { 20u8 };
            let u = if (x as i32).rem_euclid(31) < 15 { 30u8 } else { 0u8 };
            (v.saturating_add(u), v, v)
        });
        let b = frame_from(96, 64, |x, y| {
            let sy = y as i32 - 3;
            let v = if sy.rem_euclid(23) < 11 { 240u8 } else { 20u8 };
            let u = if (x as i32).rem_euclid(31) < 15 { 30u8 } else { 0u8 };
            (v.saturating_add(u), v, v)
        });
        let vectors = block_motion(&a, &b);
        // Same edge story on the vertical axis: the bottom block row is flush
        // with the frame and cannot search downward.
        let found = vectors.iter().map(|v| (v.dx, v.dy)).collect::<Vec<_>>();
        let agree = vectors.iter().filter(|v| v.dy == 3.0 && v.dx == 0.0).count();
        assert!(
            agree * 2 > vectors.len(),
            "most blocks should report dy=+3, dx=0; got {found:?}"
        );
        assert!(
            vectors.iter().all(|v| v.dy >= 0.0),
            "nothing moved upward; got {found:?}"
        );
    }

    #[test]
    fn motion_vector_magnitude_and_angle_are_polar_coordinates() {
        let v = MotionVector { dx: 3.0, dy: 4.0 };
        assert!((v.magnitude() - 5.0).abs() < 1e-5);
        let v = MotionVector { dx: 0.0, dy: 2.0 };
        assert!((v.angle() - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    #[test]
    fn zero_motion_lands_entirely_in_the_first_flow_bin() {
        let hist = flow_histogram(&vec![MotionVector { dx: 0.0, dy: 0.0 }; 12]);
        assert_eq!(hist.len(), FLOW_BINS);
        assert!((hist[0] - 1.0).abs() < 1e-5, "all mass in bin 0: {hist:?}");
        assert!(hist[1..].iter().all(|&h| h == 0.0));
    }

    #[test]
    fn flow_histogram_normalizes_to_one() {
        let vectors = vec![
            MotionVector { dx: 0.0, dy: 0.0 },
            MotionVector { dx: 4.0, dy: 0.0 },
            MotionVector { dx: 8.0, dy: 0.0 },
        ];
        let hist = flow_histogram(&vectors);
        let total: f32 = hist.iter().sum();
        assert!((total - 1.0).abs() < 1e-5, "histogram sums to {total}");
    }

    #[test]
    fn flow_histogram_of_no_vectors_is_all_zeros() {
        assert_eq!(flow_histogram(&[]), vec![0.0; FLOW_BINS]);
    }

    #[test]
    fn motion_stats_average_per_frame_magnitude() {
        let still = vec![MotionVector { dx: 0.0, dy: 0.0 }; 4];
        let moving = vec![MotionVector { dx: 3.0, dy: 4.0 }; 4]; // magnitude 5
        let (mean, std, max) = motion_stats(&[still, moving]);
        assert!((mean - 2.5).abs() < 1e-4, "mean {mean}");
        assert!((std - 2.5).abs() < 1e-4, "std {std}");
        assert!((max - 5.0).abs() < 1e-4, "max {max}");
    }

    #[test]
    fn motion_stats_of_an_empty_sequence_is_zero() {
        assert_eq!(motion_stats(&[]), (0.0, 0.0, 0.0));
    }
}
