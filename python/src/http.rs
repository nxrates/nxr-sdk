//! Blocking HTTP client targeting the NXR REST surface.
//!
//! Endpoints (per nx-rates monorepo) — all path-routed, `sym` is a path
//! segment (NOT a query param). The API accepts three symbol forms:
//! dash (`BTC-USDT`, preferred), slash (`BTC/USDT`), or numeric ticker_id.
//!   GET /v1/idx/{sym}?limit=1000       -> octet-stream of 56B IndexRecords
//!   GET /v1/ohlc/{sym}?tf=60&from=&to= -> JSON [{ts,open,..}, ...]
//!   GET /v1/bars/{sym}/{kind}          -> octet-stream of 96B Bars
//!   GET /v1/tickers, /v1/providers, /v1/symbols (JSON catalog)
//!
//! The client deliberately runs `reqwest::blocking` so it can be used from
//! plain Python scripts without an asyncio event loop. `py.allow_threads`
//! releases the GIL across the network call so other Python threads make
//! progress while the request is in flight.

use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

use reqwest::blocking::Client as RClient;

#[pyclass(module = "nxr_sdk._native")]
pub struct Client {
    inner: RClient,
    base: String,
}

#[pymethods]
impl Client {
    /// Construct a client pointing at the NXR REST root, e.g.
    /// `http://nxr-svc:80` or `http://nxr.nxrates.com`. `timeout` defaults
    /// to 30 seconds.
    #[new]
    #[pyo3(signature = (base_url, timeout_s=30.0))]
    fn new(base_url: &str, timeout_s: f64) -> PyResult<Self> {
        let inner = RClient::builder()
            .timeout(Duration::from_secs_f64(timeout_s.max(0.001)))
            .user_agent(concat!("nxr-sdk-python/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| PyRuntimeError::new_err(format!("reqwest build: {e}")))?;
        Ok(Self { inner, base: base_url.trim_end_matches('/').to_string() })
    }

    /// Fetch raw IndexRecord stream from `/v1/idx/{sym}`.
    ///
    /// Returns a NumPy structured array of 56B records (same dtype as
    /// `index_record_dtype()`). `sym` is required (dash/slash/ticker_id form);
    /// `provider`, `from_ms`, `to_ms`, `limit` filter the server-side scan.
    #[pyo3(signature = (sym, provider=None, from_ms=None, to_ms=None, limit=None))]
    fn fetch_idx(
        &self,
        py: Python<'_>,
        sym: &str,
        provider: Option<u16>,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: Option<usize>,
    ) -> PyResult<PyObject> {
        let mut url = format!("{}/v1/idx/{}", self.base, url_sym(sym));
        append_query(
            &mut url,
            &[
                ("provider", provider.map(|v| v.to_string())),
                ("from", from_ms.map(|v| v.to_string())),
                ("to", to_ms.map(|v| v.to_string())),
                ("limit", limit.map(|v| v.to_string())),
            ],
        );
        let bytes = py.allow_threads(|| -> Result<Vec<u8>, String> {
            let resp = self
                .inner
                .get(&url)
                .header(reqwest::header::ACCEPT, "application/octet-stream")
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{} -> {}", url, resp.status()));
            }
            let b = resp.bytes().map_err(|e| e.to_string())?;
            Ok(b.to_vec())
        })
        .map_err(PyRuntimeError::new_err)?;
        let buf = PyBytes::new_bound(py, &bytes);
        crate::decoders::decode_idx_bytes(py, &buf)
    }

    /// Fetch JSON OHLC candles from `/v1/ohlc/{sym}?tf=...`.
    ///
    /// Returns a NumPy structured array with dtype
    /// `[(ts, i8), (open, f8), (high, f8), (low, f8), (close, f8),
    /// (vbid, u8), (vask, u8), (n, u4)]`.
    #[pyo3(signature = (sym, tf, from_ms=None, to_ms=None, limit=None))]
    fn fetch_ohlc(
        &self,
        py: Python<'_>,
        sym: &str,
        tf: u32,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: Option<usize>,
    ) -> PyResult<PyObject> {
        let mut url = format!("{}/v1/ohlc/{}", self.base, url_sym(sym));
        append_query(
            &mut url,
            &[
                ("tf", Some(tf.to_string())),
                ("from", from_ms.map(|v| v.to_string())),
                ("to", to_ms.map(|v| v.to_string())),
                ("limit", limit.map(|v| v.to_string())),
            ],
        );

        let body = py.allow_threads(|| -> Result<String, String> {
            let resp = self.inner.get(&url).send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{} -> {}", url, resp.status()));
            }
            resp.text().map_err(|e| e.to_string())
        })
        .map_err(PyRuntimeError::new_err)?;

        // Parse as a JSON array of objects with keys: ts/o/h/l/c/vbid/vask/n
        // (accept several common spellings).
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| PyRuntimeError::new_err(format!("json parse: {e}")))?;
        let arr = parsed.as_array()
            .ok_or_else(|| PyRuntimeError::new_err("/v1/ohlc response is not an array"))?;

        let numpy = py.import_bound("numpy")?;
        let dtype_def = PyList::empty_bound(py);
        for (n, f) in &[("ts","<i8"),("open","<f8"),("high","<f8"),("low","<f8"),("close","<f8"),("vbid","<u8"),("vask","<u8"),("n","<u4")] {
            let t = pyo3::types::PyTuple::new_bound(py, &[
                (*n).to_object(py),
                (*f).to_object(py),
            ]);
            dtype_def.append(t)?;
        }
        let dtype = numpy.getattr("dtype")?.call1((dtype_def,))?;
        let kwargs = PyDict::new_bound(py);
        kwargs.set_item("dtype", dtype)?;
        let zeros = numpy.getattr("zeros")?.call((arr.len(),), Some(&kwargs))?;
        for (i, row) in arr.iter().enumerate() {
            let ts = row.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
            let o = num_f(row, &["open","o"]);
            let h = num_f(row, &["high","h"]);
            let l = num_f(row, &["low","l"]);
            let c = num_f(row, &["close","c"]);
            let vbid = row.get("vbid").and_then(|v| v.as_u64()).unwrap_or(0);
            let vask = row.get("vask").and_then(|v| v.as_u64()).unwrap_or(0);
            let nticks = row.get("n").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let item = zeros.get_item(i)?;
            item.set_item("ts", ts)?;
            item.set_item("open", o)?;
            item.set_item("high", h)?;
            item.set_item("low", l)?;
            item.set_item("close", c)?;
            item.set_item("vbid", vbid)?;
            item.set_item("vask", vask)?;
            item.set_item("n", nticks)?;
        }
        Ok(zeros.unbind())
    }

    /// Fetch raw Bar stream from `/v1/bars/{sym}/{kind}`. Returns NumPy
    /// structured array of 96B records (dtype = `bar_dtype()`).
    /// `kind` is `kline` or `renko`.
    #[pyo3(signature = (sym, kind, from_ms=None, to_ms=None, limit=None))]
    fn fetch_bars(
        &self,
        py: Python<'_>,
        sym: &str,
        kind: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: Option<usize>,
    ) -> PyResult<PyObject> {
        let mut url = format!("{}/v1/bars/{}/{}", self.base, url_sym(sym), kind);
        append_query(
            &mut url,
            &[
                ("from", from_ms.map(|v| v.to_string())),
                ("to", to_ms.map(|v| v.to_string())),
                ("limit", limit.map(|v| v.to_string())),
            ],
        );
        let bytes = py.allow_threads(|| -> Result<Vec<u8>, String> {
            let resp = self
                .inner
                .get(&url)
                .header(reqwest::header::ACCEPT, "application/octet-stream")
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{} -> {}", url, resp.status()));
            }
            let b = resp.bytes().map_err(|e| e.to_string())?;
            Ok(b.to_vec())
        })
        .map_err(PyRuntimeError::new_err)?;
        let buf = PyBytes::new_bound(py, &bytes);
        crate::decoders::decode_bar_bytes(py, &buf)
    }

    /// GET /v1/tickers — JSON list passthrough.
    fn fetch_tickers(&self, py: Python<'_>) -> PyResult<PyObject> {
        self.get_json(py, "/v1/tickers")
    }

    /// GET /v1/providers — JSON {id: name} passthrough.
    fn fetch_providers(&self, py: Python<'_>) -> PyResult<PyObject> {
        self.get_json(py, "/v1/providers")
    }

    /// GET /v1/symbols — JSON {symbol: ticker_id} passthrough.
    fn fetch_symbols(&self, py: Python<'_>) -> PyResult<PyObject> {
        self.get_json(py, "/v1/symbols")
    }

    fn __repr__(&self) -> String {
        format!("Client(base_url={:?})", self.base)
    }
}

impl Client {
    fn get_json(&self, py: Python<'_>, path: &str) -> PyResult<PyObject> {
        let url = format!("{}{}", self.base, path);
        let body = py.allow_threads(|| -> Result<String, String> {
            let resp = self.inner.get(&url).send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{} -> {}", url, resp.status()));
            }
            resp.text().map_err(|e| e.to_string())
        })
        .map_err(PyRuntimeError::new_err)?;
        let json_mod = py.import_bound("json")?;
        Ok(json_mod.getattr("loads")?.call1((body,))?.unbind())
    }
}

/// Convert any symbol form to the URL-safe dash form the NXR API prefers.
/// `BTC/USDT` → `BTC-USDT`; dash form and numeric ticker_id pass through
/// unchanged. Matches the server's 3-form `resolve_sym`.
fn url_sym(sym: &str) -> String {
    sym.replace('/', "-")
}

/// Append present (`Some`) params to `url` as a query string. Skips `None`.
fn append_query(url: &mut String, params: &[(&str, Option<String>)]) {
    let mut first = true;
    for (k, v) in params {
        if let Some(val) = v {
            url.push(if first { '?' } else { '&' });
            first = false;
            url.push_str(k);
            url.push('=');
            url.push_str(val);
        }
    }
}

fn num_f(row: &serde_json::Value, keys: &[&str]) -> f64 {
    for k in keys {
        if let Some(v) = row.get(*k) {
            if let Some(f) = v.as_f64() { return f; }
            if let Some(i) = v.as_i64() { return i as f64; }
        }
    }
    f64::NAN
}
