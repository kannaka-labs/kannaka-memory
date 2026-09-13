//! Color analysis utilities: RGB→HSV, histograms, dominant colors.

use super::HSV_BINS;

/// HSV color (H: 0-360, S: 0-1, V: 0-1).
#[derive(Debug, Clone, Copy)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

/// Convert RGB (0-255) to HSV.
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> Hsv {
    let rf = r as f32 / 255.0;
    let gf = g as f32 / 255.0;
    let bf = b as f32 / 255.0;

    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;

    let v = max;
    let s = if max > 0.0 { delta / max } else { 0.0 };

    let h = if delta < 1e-6 {
        0.0
    } else if (max - rf).abs() < 1e-6 {
        60.0 * (((gf - bf) / delta) % 6.0)
    } else if (max - gf).abs() < 1e-6 {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };

    let h = if h < 0.0 { h + 360.0 } else { h };

    Hsv { h, s, v }
}

/// Compute HSV histogram from a frame's RGB pixels.
/// Returns `HSV_BINS * 3` bins (H, S, V each with HSV_BINS bins).
pub fn hsv_histogram(rgb: &[u8], width: u32, height: u32) -> Vec<f32> {
    let npx = (width * height) as usize;
    let mut h_hist = vec![0.0f32; HSV_BINS];
    let mut s_hist = vec![0.0f32; HSV_BINS];
    let mut v_hist = vec![0.0f32; HSV_BINS];

    for i in 0..npx {
        let base = i * 3;
        let hsv = rgb_to_hsv(rgb[base], rgb[base + 1], rgb[base + 2]);

        let h_bin = ((hsv.h / 360.0 * HSV_BINS as f32) as usize).min(HSV_BINS - 1);
        let s_bin = ((hsv.s * HSV_BINS as f32) as usize).min(HSV_BINS - 1);
        let v_bin = ((hsv.v * HSV_BINS as f32) as usize).min(HSV_BINS - 1);

        h_hist[h_bin] += 1.0;
        s_hist[s_bin] += 1.0;
        v_hist[v_bin] += 1.0;
    }

    // Normalize
    let total = npx as f32;
    for v in h_hist.iter_mut().chain(s_hist.iter_mut()).chain(v_hist.iter_mut()) {
        *v /= total;
    }

    let mut result = Vec::with_capacity(HSV_BINS * 3);
    result.extend_from_slice(&h_hist);
    result.extend_from_slice(&s_hist);
    result.extend_from_slice(&v_hist);
    result
}

/// Squared distance between two HSV colours, with hue treated as circular.
fn hsv_distance(a: &Hsv, b: &Hsv) -> f32 {
    let raw = (a.h / 360.0 - b.h / 360.0).abs();
    let dh = raw.min(1.0 - raw);
    let ds = a.s - b.s;
    let dv = a.v - b.v;
    dh * dh + ds * ds + dv * dv
}

/// Extract top-K dominant colors via simple k-means in HSV space.
/// Returns K * 3 values (H, S, V for each centroid), normalized to 0-1 range,
/// most populous cluster first.
pub fn dominant_colors(rgb: &[u8], width: u32, height: u32, k: usize) -> Vec<f32> {
    let npx = (width * height) as usize;
    if npx == 0 || k == 0 {
        return vec![0.0; k * 3];
    }

    // Sample pixels for efficiency (max ~1000)
    let step = (npx / 1000).max(1);
    let mut samples: Vec<Hsv> = Vec::new();
    for i in (0..npx).step_by(step) {
        let base = i * 3;
        if base + 2 >= rgb.len() {
            break;
        }
        samples.push(rgb_to_hsv(rgb[base], rgb[base + 1], rgb[base + 2]));
    }
    if samples.is_empty() {
        return vec![0.0; k * 3];
    }

    // Farthest-point initialization: take the first sample, then repeatedly
    // take the sample furthest from everything chosen so far.
    //
    // Seeding at evenly spaced *indices* instead looks reasonable and is a
    // trap: samples arrive in row-major order, so the stride
    // `samples.len() / k` lines up with the row period on any frame with
    // vertical structure. A frame that is red on the left and blue on the
    // right seeded all four centroids on red pixels — and identical centroids
    // send every sample to cluster 0, collapsing k-means to a single average
    // colour. Half the frame vanished from its own dominant-colour list.
    let mut centroids: Vec<Hsv> = vec![samples[0]];
    while centroids.len() < k {
        let mut best_i = 0usize;
        let mut best_d = -1.0f32;
        for (i, s) in samples.iter().enumerate() {
            let d = centroids
                .iter()
                .map(|c| hsv_distance(s, c))
                .fold(f32::MAX, f32::min);
            if d > best_d {
                best_d = d;
                best_i = i;
            }
        }
        // Every remaining sample already sits on a centroid: the frame has
        // fewer distinct colours than k. Stop rather than duplicate.
        if best_d <= 0.0 {
            break;
        }
        centroids.push(samples[best_i]);
    }

    // K-means iterations
    let kc = centroids.len();
    let mut counts = vec![0usize; kc];
    for _ in 0..10 {
        let mut sums = vec![(0.0f32, 0.0f32, 0.0f32); kc];
        counts = vec![0usize; kc];

        for s in &samples {
            let mut best = 0;
            let mut best_dist = f32::MAX;
            for (ci, c) in centroids.iter().enumerate() {
                let dist = hsv_distance(s, c);
                if dist < best_dist {
                    best_dist = dist;
                    best = ci;
                }
            }
            sums[best].0 += s.h;
            sums[best].1 += s.s;
            sums[best].2 += s.v;
            counts[best] += 1;
        }

        for i in 0..kc {
            if counts[i] > 0 {
                let n = counts[i] as f32;
                centroids[i] = Hsv {
                    h: sums[i].0 / n,
                    s: sums[i].1 / n,
                    v: sums[i].2 / n,
                };
            }
        }
    }

    // Most dominant first means most PIXELS first. Sorting by V, as this used
    // to, ordered by brightness and called it dominance — so a one-pixel
    // highlight outranked the colour covering the frame.
    let mut order: Vec<usize> = (0..kc).collect();
    order.sort_by(|&a, &b| {
        counts[b]
            .cmp(&counts[a])
            .then(centroids[b].v.total_cmp(&centroids[a].v))
    });

    let mut result = Vec::with_capacity(k * 3);
    for &i in &order {
        if counts[i] == 0 {
            continue; // an empty cluster is not a colour of this frame
        }
        result.push(centroids[i].h / 360.0); // normalize H to 0-1
        result.push(centroids[i].s);
        result.push(centroids[i].v);
    }
    // Pad if fewer than k clusters
    while result.len() < k * 3 {
        result.push(0.0);
    }
    result.truncate(k * 3);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&[r, g, b]);
        }
        v
    }

    #[test]
    fn rgb_to_hsv_maps_the_primaries_to_their_known_hues() {
        let red = rgb_to_hsv(255, 0, 0);
        assert!(red.h.abs() < 1e-3, "red hue is 0, got {}", red.h);
        assert!((red.s - 1.0).abs() < 1e-3);
        assert!((red.v - 1.0).abs() < 1e-3);

        let green = rgb_to_hsv(0, 255, 0);
        assert!((green.h - 120.0).abs() < 1e-2, "green hue {}", green.h);

        let blue = rgb_to_hsv(0, 0, 255);
        assert!((blue.h - 240.0).abs() < 1e-2, "blue hue {}", blue.h);
    }

    #[test]
    fn rgb_to_hsv_maps_greys_to_zero_saturation() {
        for level in [0u8, 64, 128, 255] {
            let hsv = rgb_to_hsv(level, level, level);
            assert!(hsv.s.abs() < 1e-5, "grey {level} has saturation {}", hsv.s);
            assert!((hsv.v - level as f32 / 255.0).abs() < 1e-5);
        }
    }

    #[test]
    fn hsv_histogram_of_a_solid_color_puts_all_mass_in_one_bin_per_channel() {
        let hist = hsv_histogram(&solid(8, 8, 255, 0, 0), 8, 8);
        assert_eq!(hist.len(), HSV_BINS * 3);

        // Hue 0, saturation 1 (top bin), value 1 (top bin).
        assert!((hist[0] - 1.0).abs() < 1e-5, "hue bin 0: {}", hist[0]);
        assert!((hist[HSV_BINS + HSV_BINS - 1] - 1.0).abs() < 1e-5);
        assert!((hist[2 * HSV_BINS + HSV_BINS - 1] - 1.0).abs() < 1e-5);

        // Each channel normalizes to 1.
        for ch in 0..3 {
            let total: f32 = hist[ch * HSV_BINS..(ch + 1) * HSV_BINS].iter().sum();
            assert!((total - 1.0).abs() < 1e-4, "channel {ch} sums to {total}");
        }
    }

    #[test]
    fn hsv_histograms_of_different_colors_differ() {
        let red = hsv_histogram(&solid(8, 8, 255, 0, 0), 8, 8);
        let blue = hsv_histogram(&solid(8, 8, 0, 0, 255), 8, 8);
        let l1: f32 = red.iter().zip(&blue).map(|(a, b)| (a - b).abs()).sum();
        assert!(l1 > 1.0, "red and blue histograms differ by only {l1}");
    }

    #[test]
    fn dominant_colors_of_a_two_color_frame_recover_both() {
        // Left half red, right half blue.
        let (w, h) = (32u32, 16u32);
        let mut rgb = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                if x < w / 2 {
                    rgb.extend_from_slice(&[255, 0, 0]);
                } else {
                    rgb.extend_from_slice(&[0, 0, 255]);
                }
            }
        }

        let dc = dominant_colors(&rgb, w, h, 4);
        assert_eq!(dc.len(), 12, "4 centroids x HSV");

        let hues: Vec<f32> = dc.chunks(3).map(|c| c[0] * 360.0).collect();
        assert!(
            hues.iter().any(|&h| h < 15.0 || h > 345.0),
            "red should survive as a centroid: {hues:?}"
        );
        assert!(
            hues.iter().any(|&h| (h - 240.0).abs() < 15.0),
            "blue should survive as a centroid: {hues:?}"
        );
    }

    #[test]
    fn dominant_colors_are_ordered_by_how_much_of_the_frame_they_cover() {
        // 7/8 dark red, 1/8 bright white. White is brighter; red is dominant.
        let (w, h) = (64u32, 16u32);
        let mut rgb = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                if x < w * 7 / 8 {
                    rgb.extend_from_slice(&[120, 0, 0]);
                } else {
                    rgb.extend_from_slice(&[255, 255, 255]);
                }
            }
        }

        let dc = dominant_colors(&rgb, w, h, 2);
        let first = &dc[0..3];
        assert!(
            first[1] > 0.5,
            "the majority colour (saturated red) must come first, got h={} s={} v={}",
            first[0] * 360.0,
            first[1],
            first[2]
        );
    }

    #[test]
    fn dominant_colors_of_an_empty_frame_are_neutral() {
        assert_eq!(dominant_colors(&[], 0, 0, 4), vec![0.0; 12]);
    }
}
