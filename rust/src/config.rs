//! Environment-based configuration for the NXRates stack.

use std::env;

/// Single configuration struct for the entire NXRates stack.
/// All fields are loaded from environment variables with `NXR_` prefix.
#[derive(Debug, Clone)]
pub struct NxrConfig {
    pub log_level: String,
    /// TCP port the aggregator listens on for MITCH frames from broker forwarders
    pub listen_port: u16,
    /// Aggregation cycle interval in milliseconds (default: 50)
    pub aggregation_interval_ms: u64,
    /// Duration in ms after which a quote is considered stale (weight decays to zero)
    pub stale_threshold_ms: u64,
    /// Heartbeat interval in ms: when no new ticks arrive, re-emit the last index
    /// at this cadence so WS clients always have recent data. Wire-only (not stored
    /// to .idx files). Default: 1000 (1 Hz).
    pub heartbeat_interval_ms: u64,
    /// HTTP server host
    pub server_host: String,
    /// HTTP server port
    pub server_port: u16,
    /// UDP multicast group address for internal consumers (BTR, btr-ml)
    pub multicast_addr: String,
    /// UDP multicast port
    pub multicast_port: u16,
    /// Directory for mmap AppendLog files
    pub data_dir: String,
    /// Comma-separated list of symbols to subscribe to
    pub symbols: String,
    /// Host for UDP sink (aggregator target for provider Indexes).
    pub sink_host: String,
    /// Port for UDP sink (aggregator target for provider Indexes).
    pub sink_port: u16,
    /// Path to the hot-reloadable TDWAP weights / ticker parameters file.
    pub ticker_params_path: String,
}

impl NxrConfig {
    pub fn from_env() -> Self {
        Self {
            log_level: env_or("NXR_LOG_LEVEL", "info"),
            listen_port: env_or("NXR_LISTEN_PORT", "9500").parse().unwrap_or(9500),
            aggregation_interval_ms: env_or("NXR_AGGREGATION_INTERVAL_MS", "50")
                .parse()
                .unwrap_or(50),
            stale_threshold_ms: env_or("NXR_STALE_THRESHOLD_MS", "10000")
                .parse()
                .unwrap_or(10000),
            heartbeat_interval_ms: env_or("NXR_HEARTBEAT_INTERVAL_MS", "1000")
                .parse()
                .unwrap_or(1000),
            server_host: env_or("NXR_SERVER_HOST", "0.0.0.0"),
            server_port: env_or("NXR_SERVER_PORT", "40004").parse().unwrap_or(40004),
            multicast_addr: env_or("NXR_MULTICAST_ADDR", "239.0.42.1"),
            multicast_port: env_or("NXR_MULTICAST_PORT", "40006").parse().unwrap_or(40006),
            data_dir: env_or("NXR_DATA_DIR", "/data/index"),
            symbols: env_or(
                "NXR_SYMBOLS",
                "BTC/USDT,ETH/USDT,SOL/USDT,XRP/USDT,BNB/USDT,ADA/USDT,DOGE/USDT,\
                 AVAX/USDT,LINK/USDT,DOT/USDT,LTC/USDT,BCH/USDT,TRX/USDT,XMR/USDT,\
                 ZEC/USDT,SUI/USDT,HYPE/USDT,UNI/USDT,XLM/USDT,HBAR/USDT,ETC/USDT,\
                 TON/USDT,PEPE/USDT,SHIB/USDT,\
                 BTC/USD,ETH/USD,SOL/USD,XRP/USD,BNB/USD,ADA/USD,DOGE/USD,\
                 AVAX/USD,LINK/USD,DOT/USD,LTC/USD,BCH/USD,TRX/USD,XMR/USD,\
                 ZEC/USD,SUI/USD,HYPE/USD,UNI/USD,XLM/USD,HBAR/USD,ETC/USD,\
                 TON/USD,PEPE/USD,SHIB/USD,\
                 BTC/USDC,ETH/USDC,SOL/USDC,XRP/USDC,BNB/USDC,ADA/USDC,DOGE/USDC,\
                 AVAX/USDC,LINK/USDC,DOT/USDC,LTC/USDC,\
                 AAVE/USDT,ARB/USDT,APT/USDT,ONDO/USDT,ENA/USDT,MNT/USDT,\
                 POL/USDT,OP/USDT,FIL/USDT,ALGO/USDT,BONK/USDT,DASH/USDT,\
                 PAXG/USDT,XAUT/USDT,GRT/USDT,PENDLE/USDT,RENDER/USDT,WLD/USDT,\
                 JUP/USDT,CAKE/USDT,ENS/USDT,MORPHO/USDT,LDO/USDT,CRV/USDT,\
                 ETHFI/USDT,RPL/USDT,CVX/USDT,EUL/USDT,INF/USDT,JTO/USDT,\
                 MET/USDT,ORCA/USDT,PUMP/USDT,XVS/USDT,LISTA/USDT,SOLV/USDT,\
                 HYPER/USDT,\
                 AAVE/USD,ARB/USD,APT/USD,ONDO/USD,ENA/USD,MNT/USD,\
                 POL/USD,OP/USD,FIL/USD,ALGO/USD,DASH/USD,\
                 PAXG/USD,XAUT/USD,GRT/USD,PENDLE/USD,RENDER/USD,WLD/USD,\
                 NEXO/USDT,SKY/USDT,FF/USDT,KMNO/USDT,BARD/USDT,CFG/USDT,\
                 PYTH/USDT,RAY/USDT,FLUID/USDT,COW/USDT,AR/USDT,ZRO/USDT,\
                 AXL/USDT,AERO/USDT,GNO/USDT,SYRUP/USDT,H/USDT,W/USDT,\
                 ZRX/USDT,1INCH/USDT,\
                 USDT/USD,USDC/USD,USDC/USDT,FDUSD/USD,USDS/USDT,USD1/USDT,USD1/USDC,\
                 GHO/USDT,GHO/USD,CRVUSD/USDT,USYC/USDC,BUIDL/USDC,USDF/USDT,\
                 RLUSD/USDT,RLUSD/USD,USDY/USDT,USDTB/USDT,USD0/USDT,\
                 AUSD/USDT,USDG/USDT,EURC/USDC,EURC/USD,USDD/USDT,PYUSD/USD",
            ),
            sink_host: env_or("NXR_SINK_HOST", "127.0.0.1"),
            sink_port: env_or("NXR_SINK_PORT", "40010").parse().unwrap_or(40010),
            ticker_params_path: env_or(
                "NXR_TICKER_PARAMS_PATH",
                "/data/config/ticker-params.json",
            ),
        }
    }

    pub fn symbol_list(&self) -> Vec<String> {
        self.symbols
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}
