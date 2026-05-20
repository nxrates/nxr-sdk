//! Welford-style rolling Pearson correlation accumulator — verbatim port of
//! `RollingCorrelation` from `~/Work/btr/sdk/src/types/synth-ohlc.ts` (lines 232-288).
//!
//! Maintains running sums `sx, sy, sxx, syy, sxy` over a sliding window of size
//! `N` paired samples (typically log-returns). `value()` returns the windowed
//! Pearson correlation, clamped to `[-0.99, 0.99]` to avoid singular covariance.
//! Returns `0.0` when `n < 2` or either variance is effectively 0.
//!
//! All operations are `O(1)`. Memory: 2·N `f64` for the ring buffers.

/// Sliding-window Pearson correlation. Fixed window size `N ≥ 2`.
#[derive(Debug)]
pub struct RollingCorrelation {
    n: usize,
    buf_x: Vec<f64>,
    buf_y: Vec<f64>,
    idx: usize,
    filled: usize,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
}

impl RollingCorrelation {
    /// New accumulator with window of `window_size` paired samples.
    /// Panics if `window_size < 2`.
    pub fn new(window_size: usize) -> Self {
        assert!(window_size >= 2, "window_size must be ≥ 2, got {window_size}");
        Self {
            n: window_size,
            buf_x: vec![0.0; window_size],
            buf_y: vec![0.0; window_size],
            idx: 0,
            filled: 0,
            sx: 0.0,
            sy: 0.0,
            sxx: 0.0,
            syy: 0.0,
            sxy: 0.0,
        }
    }

    /// Push paired sample `(r_a, r_b)` — typically log-returns. `O(1)`.
    /// Non-finite inputs are silently dropped (no slot consumed).
    #[inline]
    pub fn add(&mut self, r_a: f64, r_b: f64) {
        if !r_a.is_finite() || !r_b.is_finite() {
            return;
        }
        if self.filled == self.n {
            let ox = self.buf_x[self.idx];
            let oy = self.buf_y[self.idx];
            self.sx -= ox;
            self.sy -= oy;
            self.sxx -= ox * ox;
            self.syy -= oy * oy;
            self.sxy -= ox * oy;
        } else {
            self.filled += 1;
        }
        self.buf_x[self.idx] = r_a;
        self.buf_y[self.idx] = r_b;
        self.sx += r_a;
        self.sy += r_b;
        self.sxx += r_a * r_a;
        self.syy += r_b * r_b;
        self.sxy += r_a * r_b;
        self.idx = (self.idx + 1) % self.n;
    }

    /// Current Pearson correlation, clamped to `[-0.99, 0.99]`.
    /// Returns `0.0` when fewer than 2 samples or either variance is non-positive.
    pub fn value(&self) -> f64 {
        let n = self.filled as f64;
        if self.filled < 2 {
            return 0.0;
        }
        let num = n * self.sxy - self.sx * self.sy;
        let dx = n * self.sxx - self.sx * self.sx;
        let dy = n * self.syy - self.sy * self.sy;
        if !(dx > 0.0) || !(dy > 0.0) {
            return 0.0;
        }
        let rho = num / (dx * dy).sqrt();
        if !rho.is_finite() {
            return 0.0;
        }
        if rho > 0.99 {
            return 0.99;
        }
        if rho < -0.99 {
            return -0.99;
        }
        rho
    }

    /// Number of samples currently in the window (`≤ window_size`).
    #[inline]
    pub fn count(&self) -> usize {
        self.filled
    }

    /// Window size `N`.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.n
    }
}
