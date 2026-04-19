//! Binary direction labels for supervised training.
//!
//! Convention: `mode = "lo"` labels future_ret > threshold, `"so"` labels
//! future_ret < -threshold. Tail bars (last `horizon_bars`) return `None`.

/// Compute binary direction labels over a close-price series.
///
/// Returns `Vec<Option<i32>>` with `None` at the tail (insufficient forward
/// horizon) and `Some(0 or 1)` otherwise.
pub fn compute_labels(
    closes: &[f64],
    horizon_bars: usize,
    threshold: f64,
    mode: &str,
) -> Vec<Option<i32>> {
    let n = closes.len();
    let mut labels = Vec::with_capacity(n);
    for i in 0..n {
        if i + horizon_bars >= n || closes[i] <= 0.0 {
            labels.push(None);
        } else {
            let future_ret = closes[i + horizon_bars] / closes[i] - 1.0;
            let label = match mode {
                "lo" if future_ret > threshold => 1,
                "so" if future_ret < -threshold => 1,
                _ => 0,
            };
            labels.push(Some(label));
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_labels_positive_moves() {
        let closes = [100.0, 101.0, 102.0, 99.0, 105.0, 106.0];
        let labels = compute_labels(&closes, 1, 0.005, "lo");
        assert_eq!(labels[0], Some(1)); // 101/100 - 1 = 0.01 > 0.005
        assert_eq!(labels[1], Some(1));
        assert_eq!(labels[2], Some(0)); // 99/102 < 0
        assert_eq!(labels[5], None);    // tail
    }

    #[test]
    fn short_labels_negative_moves() {
        let closes = [100.0, 99.0, 98.0, 102.0];
        let labels = compute_labels(&closes, 1, 0.005, "so");
        assert_eq!(labels[0], Some(1));
        assert_eq!(labels[2], Some(0));
    }
}
