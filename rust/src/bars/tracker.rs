//! Lightweight Renko state tracker for feature extraction.
//!
//! Unlike [`super::renko::RenkoGenerator`] (adaptive brick size from
//! Parkinson sigma, emits bars), this tracker uses a fixed brick percentage
//! and exposes smoothed state as input features for a trading model.
//!
//! 4 features per scale x 3 scales = 12 features total.
//! EMA smoothing keeps features stable under GBM subsampling.

const EMA_PERIOD: f64 = 20.0;
const EMA_ALPHA: f64 = 2.0 / (EMA_PERIOD + 1.0);

/// Single-scale Renko tracker with fixed brick_pct.
struct RenkoTracker {
    brick_pct: f64,
    level: f64,
    direction: i8,
    streak: u32,
    total_bricks: u64,
    first_ts: u64,
    last_brick_ts: u64,
    ema_direction: f64,
    ema_streak: f64,
    recent_bricks: u32,
    rate_window: usize,
    bar_idx: usize,
    brick_at_bar: Vec<bool>,
    brick_size_sum: f64,
    brick_size_count: u64,
}

impl RenkoTracker {
    fn new(brick_pct: f64, rate_window: usize) -> Self {
        Self {
            brick_pct,
            level: 0.0,
            direction: 0,
            streak: 0,
            total_bricks: 0,
            first_ts: 0,
            last_brick_ts: 0,
            ema_direction: 0.0,
            ema_streak: 0.0,
            recent_bricks: 0,
            rate_window,
            bar_idx: 0,
            brick_at_bar: vec![false; rate_window],
            brick_size_sum: 0.0,
            brick_size_count: 0,
        }
    }

    fn update(&mut self, price: f64, ts: u64) -> u32 {
        if self.level == 0.0 {
            self.level = price;
            self.first_ts = ts;
            self.last_brick_ts = ts;
            return 0;
        }

        let mut bricks_formed = 0u32;
        let brick_size = self.level * self.brick_pct;
        if brick_size <= 0.0 {
            return 0;
        }

        while price >= self.level + brick_size {
            self.level += brick_size;
            if self.direction == 1 { self.streak += 1; } else { self.direction = 1; self.streak = 1; }
            self.total_bricks += 1;
            self.last_brick_ts = ts;
            bricks_formed += 1;
            self.brick_size_sum += brick_size;
            self.brick_size_count += 1;
            let bs_new = self.level * self.brick_pct;
            if bs_new <= 0.0 { break; }
        }

        let brick_size = self.level * self.brick_pct;
        if brick_size > 0.0 {
            while price <= self.level - brick_size {
                self.level -= brick_size;
                if self.direction == -1 { self.streak += 1; } else { self.direction = -1; self.streak = 1; }
                self.total_bricks += 1;
                self.last_brick_ts = ts;
                bricks_formed += 1;
                self.brick_size_sum += brick_size;
                self.brick_size_count += 1;
                let bs_new = self.level * self.brick_pct;
                if bs_new <= 0.0 { break; }
            }
        }

        let slot = self.bar_idx % self.rate_window;
        if self.brick_at_bar[slot] {
            self.recent_bricks = self.recent_bricks.saturating_sub(1);
        }
        self.brick_at_bar[slot] = bricks_formed > 0;
        if bricks_formed > 0 {
            self.recent_bricks += 1;
        }
        self.bar_idx += 1;

        let dir_val = self.direction as f64;
        let streak_val = (1.0 + self.streak as f64).ln();
        if self.bar_idx == 1 {
            self.ema_direction = dir_val;
            self.ema_streak = streak_val;
        } else {
            self.ema_direction += EMA_ALPHA * (dir_val - self.ema_direction);
            self.ema_streak += EMA_ALPHA * (streak_val - self.ema_streak);
        }

        bricks_formed
    }

    fn features(&self) -> [f64; 4] {
        let brick_size = if self.level > 0.0 { self.level * self.brick_pct } else { 1.0 };
        let rate = if self.bar_idx > 0 {
            let window = self.bar_idx.min(self.rate_window);
            self.recent_bricks as f64 / window as f64
        } else {
            0.0
        };
        let avg_brick = if self.brick_size_count > 0 {
            self.brick_size_sum / self.brick_size_count as f64
        } else {
            brick_size
        };
        let compression = if avg_brick > 0.0 { brick_size / avg_brick } else { 1.0 };
        [self.ema_direction, self.ema_streak, rate, compression]
    }
}

/// Multi-scale Renko feature extractor (3 trackers, 4 features each).
pub struct RenkoFeatureExtractor {
    trackers: [RenkoTracker; 3],
}

/// Names of the 12 features emitted by [`RenkoFeatureExtractor::features`].
pub fn renko_feature_names() -> Vec<String> {
    let scales = ["tight", "med", "wide"];
    let components = ["ema_dir", "ema_streak", "rate", "compress"];
    let mut names = Vec::with_capacity(12);
    for scale in &scales {
        for comp in &components {
            names.push(format!("renko_{scale}_{comp}"));
        }
    }
    names
}

impl RenkoFeatureExtractor {
    /// Create with 3 brick-size scales as percentage of price.
    pub fn new(scales: [f64; 3]) -> Self {
        let rate_window = 60;
        Self {
            trackers: [
                RenkoTracker::new(scales[0], rate_window),
                RenkoTracker::new(scales[1], rate_window),
                RenkoTracker::new(scales[2], rate_window),
            ],
        }
    }

    /// Scales tuned for volatile crypto (BTC, ETH, BNB): 0.2%, 0.5%, 1.5%.
    pub fn default_crypto() -> Self {
        Self::new([0.002, 0.005, 0.015])
    }

    pub fn update(&mut self, price: f64, ts: u64) {
        for t in &mut self.trackers {
            t.update(price, ts);
        }
    }

    pub fn features(&self) -> [f64; 12] {
        let mut out = [0.0; 12];
        for (k, t) in self.trackers.iter().enumerate() {
            let f = t.features();
            out[k * 4] = f[0];
            out[k * 4 + 1] = f[1];
            out[k * 4 + 2] = f[2];
            out[k * 4 + 3] = f[3];
        }
        out
    }
}

/// Compute 12 Renko state features for an entire close-price series.
/// Returns row-major features, stride 12.
pub fn compute_renko_features(closes: &[f64], timestamps: &[u64]) -> Vec<f64> {
    let mut extractor = RenkoFeatureExtractor::default_crypto();
    let n = closes.len();
    let mut features = Vec::with_capacity(n * 12);
    for i in 0..n {
        extractor.update(closes[i], timestamps[i]);
        features.extend_from_slice(&extractor.features());
    }
    features
}
