//! Environment-based configuration for the NXRates stack.
//!
//! Storage taxonomy (all env-overrideable, all relative to `NXR_DATA_ROOT`):
//!
//! | Env var              | Default          | Purpose                                     |
//! |----------------------|------------------|---------------------------------------------|
//! | `NXR_DATA_ROOT`      | `/data`          | Parent directory for all other data dirs    |
//! | `NXR_DATA_TICKS`     | `<root>/ticks`   | Raw ingested ticks (generic, consumer-free) |
//! | `NXR_DATA_BARS`      | `<root>/bars`    | Aggregated bars (.bars, consumer-free)      |
//! | `NXR_DATA_INDEXES`   | `<root>/indexes` | Aggregator Index `.idx` AppendLogs          |
//! | `NXR_DATA_CONFIG`    | `<root>/config`  | Hot-reload weights, ticker params           |
//!
//! Individual env vars override the root-derived default. BTR-specific results
//! live in `/data/btr` controlled by `BTR_RESULTS_DIR` (outside NxrConfig).

use std::env;

/// Single configuration struct for the entire NXRates stack.
/// All fields are loaded from environment variables with `NXR_` prefix.
#[derive(Debug, Clone)]
pub struct NxrConfig {
    pub log_level: String,
    /// TCP port the aggregator listens on for MITCH frames from provider forwarders
    pub listen_port: u16,
    /// Aggregation cycle interval in milliseconds (default: 200 = 5 Hz)
    pub aggregation_interval_ms: u64,
    /// Duration in ms after which a quote is considered stale (weight decays to zero)
    pub stale_threshold_ms: u64,
    /// TDWAP per-provider decay-weight recompute cadence, in ms. The throttle
    /// freezes the normalized weight vector between refreshes so the emitted
    /// composite Index is byte-identical on a quiet (flat-quote) pair — the
    /// `.idx` 5-field delta-gate then suppresses the redundant write, which is
    /// what gives the on-disk diff-compression (otherwise sub-ULP decay drift
    /// every cycle defeats the gate and quiet pairs write every cycle, up to
    /// ~3× the disk). `None` ⇒ derive from `stale_threshold_ms`
    /// (`default_refresh_interval_ms`). Any value (derived or explicit) is hard-
    /// floored at `3 × aggregation_interval_ms` so the throttle can never
    /// collapse to per-cycle. Override via `NXR_WEIGHT_REFRESH_MS`.
    pub weight_refresh_ms: Option<u64>,
    /// Heartbeat interval in ms: when no new ticks arrive, re-emit the last index
    /// at this cadence so WS clients always have recent data. Also drives idx
    /// liveness sentinels on disk (via IdxShardWriter). Default: 1000 (1 Hz).
    pub heartbeat_interval_ms: u64,
    /// HTTP server host
    pub server_host: String,
    /// HTTP server port
    pub server_port: u16,
    /// UDP multicast group address for feed A (CME-style redundant feed).
    /// Publisher sends every frame to both A and B; consumers dedupe by
    /// `(mts, sequence)` from the MitchHeader. Running two groups on different
    /// LAN paths survives single-packet loss on one leg.
    pub multicast_addr_a: String,
    /// UDP multicast port for feed A.
    pub multicast_port_a: u16,
    /// UDP multicast group address for feed B (redundant peer of feed A).
    pub multicast_addr_b: String,
    /// UDP multicast port for feed B.
    pub multicast_port_b: u16,
    /// Raw tick cache (exchange dumps, provider tick recordings).
    pub ticks_dir: String,
    /// Generated bar files (.bars) produced by series-factory.
    pub bars_dir: String,
    /// Aggregator Index AppendLog directory (.idx files per ticker).
    pub indexes_dir: String,
    /// Comma-separated list of symbols to subscribe to
    pub symbols: String,
    /// Host for UDP sink (aggregator target for provider Indexes).
    pub sink_host: String,
    /// Port for UDP sink (aggregator target for provider Indexes).
    pub sink_port: u16,
    /// Path to the hot-reloadable TDWAP weights / ticker parameters file.
    /// Defaults to `<config_dir>/ticker-params.json`.
    pub ticker_params_path: String,
    /// Per-ticker absolute cap on any single provider's share of total weight.
    /// Prevents a single wash-volume venue (eg a tier-3 exchange spiking 100× the
    /// typical volume on an illiquid alt) from dominating consolidated TDWAP.
    /// Bloomberg BVAL tier-1 venues sit around 35-40%; this is the upper end of
    /// industry idiom, chosen to minimise accuracy cost on legit-concentrated
    /// markets while neutering wash-vol capture. Tunable per deployment.
    /// Default: 0.40 (40%).
    pub max_weight_per_source: f64,
    /// Minimum number of active providers per ticker required to enforce the
    /// cap. With < N providers, the thin coverage is itself the bottleneck and
    /// forcing a cap would distort the few legitimate quotes. Default: 3.
    pub min_providers_for_cap: usize,
    /// Anomaly flag threshold: when raw_volume(p, t) > anomaly_vol_ratio ·
    /// median_t for a ticker, log a `weights_anomaly_high_vol` warn. Useful for
    /// spotting CMC scrape errors or genuine wash-trading. Default: 5.0.
    pub anomaly_vol_ratio: f64,
}

impl NxrConfig {
    pub fn from_env() -> Self {
        let data_root = env_or("NXR_DATA_ROOT", "/data");
        let ticks_dir = env_or("NXR_DATA_TICKS", &format!("{}/ticks", data_root));
        let bars_dir = env_or("NXR_DATA_BARS", &format!("{}/bars", data_root));
        let indexes_dir = env_or("NXR_DATA_INDEXES", &format!("{}/indexes", data_root));
        let config_dir = env_or("NXR_DATA_CONFIG", &format!("{}/config", data_root));
        let ticker_params_path = env_or(
            "NXR_TICKER_PARAMS_PATH",
            &format!("{}/ticker-params.json", config_dir),
        );

        Self {
            log_level: env_or("NXR_LOG_LEVEL", "info"),
            listen_port: env_or("NXR_LISTEN_PORT", "9500").parse().unwrap_or(9500),
            aggregation_interval_ms: env_or("NXR_AGGREGATION_INTERVAL_MS", "200")
                .parse()
                .unwrap_or(200),
            stale_threshold_ms: env_or("NXR_STALE_THRESHOLD_MS", "10000")
                .parse()
                .unwrap_or(10000),
            // None ⇒ derive from stale_threshold_ms; explicit value is hard-
            // floored at 3× aggregation_interval_ms in the aggregator.
            weight_refresh_ms: env_opt("NXR_WEIGHT_REFRESH_MS"),
            heartbeat_interval_ms: env_or("NXR_HEARTBEAT_INTERVAL_MS", "1000")
                .parse()
                .unwrap_or(1000),
            server_host: env_or("NXR_SERVER_HOST", "0.0.0.0"),
            server_port: env_or("NXR_SERVER_PORT", "40004").parse().unwrap_or(40004),
            multicast_addr_a: env_or("NXR_MULTICAST_ADDR_A", "239.0.42.1"),
            multicast_port_a: env_or("NXR_MULTICAST_PORT_A", "40006").parse().unwrap_or(40006),
            multicast_addr_b: env_or("NXR_MULTICAST_ADDR_B", "239.0.42.2"),
            multicast_port_b: env_or("NXR_MULTICAST_PORT_B", "40007").parse().unwrap_or(40007),
            ticks_dir,
            bars_dir,
            indexes_dir,
            // Forwarder subscription manifest. POLICY (operator 2026-06-02): NO
            // crypto symbol (volatile OR stable) is quoted vs USD natively. All
            // `<crypto>/USD` are SYNTH, derived `<crypto>/USDT × USDT/USD` (see
            // `synth::triangulation_rules` — USDT/USD-anchored, ∵ USDT/USD has
            // 12 providers / $579M depth vs USDC/USD's 5 / $113M). The ONLY
            // native `/USD` tickers are the two stablecoin anchors USDT/USD and
            // USDC/USD (real fiat-quoted CEX books on kraken/binance/bitstamp/
            // gemini/coinbase). Every other stablecoin is quoted vs USDT (e.g.
            // PYUSD/USDT, FDUSD/USDT) — its /USD is synth-derivable on demand.
            symbols: env_or(
                "NXR_SYMBOLS",
                "BTC/USDT,ETH/USDT,SOL/USDT,XRP/USDT,BNB/USDT,ADA/USDT,DOGE/USDT,\
                 AVAX/USDT,LINK/USDT,DOT/USDT,LTC/USDT,BCH/USDT,TRX/USDT,XMR/USDT,\
                 ZEC/USDT,SUI/USDT,HYPE/USDT,UNI/USDT,XLM/USDT,HBAR/USDT,ETC/USDT,\
                 TON/USDT,PEPE/USDT,SHIB/USDT,\
                 BTC/USDC,ETH/USDC,SOL/USDC,XRP/USDC,BNB/USDC,ADA/USDC,DOGE/USDC,\
                 AVAX/USDC,LINK/USDC,DOT/USDC,LTC/USDC,\
                 AAVE/USDT,ARB/USDT,APT/USDT,ONDO/USDT,ENA/USDT,MNT/USDT,\
                 POL/USDT,OP/USDT,FIL/USDT,ALGO/USDT,BONK/USDT,DASH/USDT,\
                 PAXG/USDT,XAUT/USDT,GRT/USDT,PENDLE/USDT,RENDER/USDT,WLD/USDT,\
                 JUP/USDT,CAKE/USDT,ENS/USDT,MORPHO/USDT,LDO/USDT,CRV/USDT,\
                 ETHFI/USDT,RPL/USDT,CVX/USDT,EUL/USDT,INF/USDT,JTO/USDT,\
                 MET/USDT,ORCA/USDT,PUMP/USDT,XVS/USDT,LISTA/USDT,SOLV/USDT,\
                 HYPER/USDT,\
                 NEXO/USDT,SKY/USDT,FF/USDT,KMNO/USDT,BARD/USDT,CFG/USDT,\
                 PYTH/USDT,RAY/USDT,FLUID/USDT,COW/USDT,AR/USDT,ZRO/USDT,\
                 AXL/USDT,AERO/USDT,GNO/USDT,SYRUP/USDT,H/USDT,W/USDT,\
                 ZRX/USDT,1INCH/USDT,\
                 USDT/USD,USDC/USD,USDC/USDT,FDUSD/USDT,USDS/USDT,USD1/USDT,USD1/USDC,\
                 GHO/USDT,CRVUSD/USDT,USYC/USDC,BUIDL/USDC,USDF/USDT,\
                 RLUSD/USDT,USDY/USDT,USDTB/USDT,USD0/USDT,\
                 AUSD/USDT,USDG/USDT,EURC/USDC,USDD/USDT,PYUSD/USDT,\
                 USDE/USDT,USDE/USDC",
            ),
            sink_host: env_or("NXR_SINK_HOST", "127.0.0.1"),
            sink_port: env_or("NXR_SINK_PORT", "40010").parse().unwrap_or(40010),
            ticker_params_path,
            max_weight_per_source: env_or("NXR_MAX_WEIGHT_PER_SOURCE", "0.40")
                .parse()
                .unwrap_or(0.40),
            min_providers_for_cap: env_or("NXR_MIN_PROVIDERS_FOR_CAP", "3")
                .parse()
                .unwrap_or(3),
            anomaly_vol_ratio: env_or("NXR_ANOMALY_VOL_RATIO", "5.0")
                .parse()
                .unwrap_or(5.0),
        }
    }

    pub fn symbol_list(&self) -> Vec<String> {
        self.symbols
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Data root directory inferred from `indexes_dir` (its parent). Falls
    /// back to `/data` if `indexes_dir` is at the filesystem root. Single
    /// source of truth for the 3 inline `Path::new(&self.indexes_dir).parent()…`
    /// copies in `core/src/{main,aggregator,server/rest}.rs`.
    pub fn data_root(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.indexes_dir)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/data"))
            .to_path_buf()
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Read an optional numeric env var. `None` if unset or unparseable.
fn env_opt<T: std::str::FromStr>(key: &str) -> Option<T> {
    env::var(key).ok().and_then(|v| v.parse().ok())
}
