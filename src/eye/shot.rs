//! Shot boundary detection via histogram difference between consecutive frames.

/// Detect shot boundaries from per-frame spatial feature vectors.
///
/// Uses the color histogram portion (first 48 dims) of each frame's features.
/// A shot boundary is detected when the histogram difference exceeds a threshold.
pub fn detect_shots(per_frame_features: &[Vec<f32>]) -> Vec<usize> {
    if per_frame_features.len() < 2 {
        return vec![];
    }

    // Use first 48 dims (HSV histogram) for shot detection
    let hist_dim = 48.min(per_frame_features[0].len());
    let mut diffs: Vec<f32> = Vec::with_capacity(per_frame_features.len() - 1);

    for i in 1..per_frame_features.len() {
        let diff: f32 = (0..hist_dim)
            .map(|d| (per_frame_features[i][d] - per_frame_features[i - 1][d]).abs())
            .sum();
        diffs.push(diff);
    }

    if diffs.is_empty() {
        return vec![];
    }

    // Adaptive threshold: mean + 2*std of histogram differences
    let n = diffs.len() as f32;
    let mean = diffs.iter().sum::<f32>() / n;
    let variance = diffs.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / n;
    let std = variance.sqrt();
    let threshold = mean + 2.0 * std;

    let mut boundaries = Vec::new();
    for (i, &diff) in diffs.iter().enumerate() {
        if diff > threshold {
            boundaries.push(i + 1); // boundary is at the frame AFTER the change
        }
    }

    boundaries
}

/// Compute shot-level statistics from detected boundaries.
pub struct ShotStats {
    /// Number of shots.
    pub count: usize,
    /// Mean shot length in frames.
    pub mean_length: f32,
    /// Std of shot lengths.
    pub std_length: f32,
    /// Max shot length.
    pub max_length: f32,
    /// Regularity of cut rhythm (0=irregular, 1=perfectly regular).
    pub regularity: f32,
}

pub fn shot_statistics(boundaries: &[usize], total_frames: usize) -> ShotStats {
    if boundaries.is_empty() {
        return ShotStats {
            count: 1,
            mean_length: total_frames as f32,
            std_length: 0.0,
            max_length: total_frames as f32,
            regularity: 1.0,
        };
    }

    // Shot lengths
    let mut lengths: Vec<f32> = Vec::new();
    let mut prev = 0;
    for &b in boundaries {
        lengths.push((b - prev) as f32);
        prev = b;
    }
    lengths.push((total_frames - prev) as f32); // last shot

    let count = lengths.len();
    let mean = lengths.iter().sum::<f32>() / count as f32;
    let variance = lengths.iter().map(|l| (l - mean).powi(2)).sum::<f32>() / count as f32;
    let std = variance.sqrt();
    let max = lengths.iter().cloned().fold(0.0f32, f32::max);

    // Regularity: 1 - (std / mean), clamped to [0, 1]
    let regularity = if mean > 0.0 {
        (1.0 - std / mean).clamp(0.0, 1.0)
    } else {
        0.0
    };

    ShotStats {
        count,
        mean_length: mean,
        std_length: std,
        max_length: max,
        regularity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 192-dim feature row whose first 48 dims (the HSV histogram that shot
    /// detection reads) are a one-hot on `bin`.
    fn frame_with_color(bin: usize) -> Vec<f32> {
        let mut f = vec![0.0f32; 192];
        f[bin] = 1.0;
        f
    }

    #[test]
    fn a_hard_cut_between_two_solid_colors_yields_exactly_one_boundary() {
        // Four frames of one colour, then four of another: one cut.
        let mut frames: Vec<Vec<f32>> = (0..4).map(|_| frame_with_color(3)).collect();
        frames.extend((0..4).map(|_| frame_with_color(20)));

        let boundaries = detect_shots(&frames);
        assert_eq!(
            boundaries,
            vec![4],
            "the cut is at frame 4 (the first frame of the new shot)"
        );
    }

    #[test]
    fn two_hard_cuts_yield_two_boundaries() {
        let mut frames: Vec<Vec<f32>> = (0..4).map(|_| frame_with_color(3)).collect();
        frames.extend((0..4).map(|_| frame_with_color(20)));
        frames.extend((0..4).map(|_| frame_with_color(40)));
        assert_eq!(detect_shots(&frames), vec![4, 8]);
    }

    #[test]
    fn a_static_sequence_yields_no_boundaries() {
        let frames: Vec<Vec<f32>> = (0..10).map(|_| frame_with_color(5)).collect();
        assert!(
            detect_shots(&frames).is_empty(),
            "identical frames contain no cut"
        );
    }

    #[test]
    fn a_single_frame_yields_no_boundaries() {
        assert!(detect_shots(&[frame_with_color(1)]).is_empty());
        assert!(detect_shots(&[]).is_empty());
    }

    #[test]
    fn shot_statistics_with_no_boundaries_describe_one_whole_shot() {
        let stats = shot_statistics(&[], 40);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.mean_length, 40.0);
        assert_eq!(stats.std_length, 0.0);
        assert_eq!(stats.regularity, 1.0);
    }

    #[test]
    fn shot_statistics_split_the_clip_at_each_boundary() {
        // Boundaries at 10 and 20 over 30 frames = three shots of 10.
        let stats = shot_statistics(&[10, 20], 30);
        assert_eq!(stats.count, 3);
        assert!((stats.mean_length - 10.0).abs() < 1e-5);
        assert!((stats.std_length - 0.0).abs() < 1e-5);
        assert!((stats.max_length - 10.0).abs() < 1e-5);
        assert!(
            (stats.regularity - 1.0).abs() < 1e-5,
            "evenly spaced cuts are perfectly regular"
        );
    }

    #[test]
    fn irregular_cuts_score_below_regular_ones() {
        let regular = shot_statistics(&[10, 20], 30);
        let irregular = shot_statistics(&[2, 25], 30);
        assert!(
            irregular.regularity < regular.regularity,
            "regularity {} should be below {}",
            irregular.regularity,
            regular.regularity
        );
    }
}
