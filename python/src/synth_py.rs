//! Synth tick composition exposed to Python.
//!
//! `compute_synth_tick(legs, snapshots)` takes:
//!   - `legs`: list of (sym, exp) tuples where exp is +1 or -1
//!   - `snapshots`: dict {sym -> (bid, ask, mid, conf)} for every referenced leg
//!
//! Returns a dict `{bid, ask, mid, conf}` for the composed quote, or None if
//! any leg is missing or has a non-positive quote.

use std::collections::HashMap;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use nxr_sdk::synth::{Leg, SynthPath, LegTick, compute_synth_tick as rs_compute};

#[pyfunction]
pub fn compute_synth_tick(
    py: Python<'_>,
    legs: &Bound<'_, PyList>,
    snapshots: &Bound<'_, PyDict>,
) -> PyResult<Option<PyObject>> {
    // Parse legs.
    let mut rs_legs: Vec<Leg> = Vec::with_capacity(legs.len());
    for item in legs.iter() {
        let tup: &Bound<'_, PyTuple> = item.downcast()
            .map_err(|_| PyValueError::new_err("each leg must be a (sym, exp) tuple"))?;
        if tup.len() != 2 {
            return Err(PyValueError::new_err("each leg tuple must have 2 elements"));
        }
        let sym: String = tup.get_item(0)?.extract()?;
        let exp: i8 = tup.get_item(1)?.extract()?;
        if exp != 1 && exp != -1 {
            return Err(PyValueError::new_err(format!("leg exp must be +1 or -1, got {exp}")));
        }
        rs_legs.push(Leg::new(sym, exp));
    }
    let path = SynthPath { sym: String::new(), legs: rs_legs };

    // Parse snapshots into a Rust HashMap keyed on the leg sym str.
    // We need the HashMap to hold &str keys whose backing String outlives the
    // call; collect into a Vec<(String, LegTick)> first.
    let mut owned: Vec<(String, LegTick)> = Vec::with_capacity(snapshots.len());
    for (k, v) in snapshots.iter() {
        let sym: String = k.extract()?;
        let snap = if let Ok(tup) = v.downcast::<PyTuple>() {
            // (bid, ask, mid, conf) — conf optional
            let bid: f64 = tup.get_item(0)?.extract()?;
            let ask: f64 = tup.get_item(1)?.extract()?;
            let mid: f64 = if tup.len() > 2 { tup.get_item(2)?.extract()? } else { (bid + ask) / 2.0 };
            let conf: u16 = if tup.len() > 3 { tup.get_item(3)?.extract()? } else { 10_000 };
            LegTick { bid, ask, mid, conf }
        } else if let Ok(d) = v.downcast::<PyDict>() {
            let bid: f64 = d.get_item("bid")?.ok_or_else(|| PyValueError::new_err("snapshot missing bid"))?.extract()?;
            let ask: f64 = d.get_item("ask")?.ok_or_else(|| PyValueError::new_err("snapshot missing ask"))?.extract()?;
            let mid: f64 = match d.get_item("mid")? { Some(m) => m.extract()?, None => (bid + ask) / 2.0 };
            let conf: u16 = match d.get_item("conf")? { Some(c) => c.extract()?, None => 10_000 };
            LegTick { bid, ask, mid, conf }
        } else {
            return Err(PyValueError::new_err(format!("snapshot for {sym} must be tuple or dict")));
        };
        owned.push((sym, snap));
    }
    let map: HashMap<&str, LegTick> = owned.iter().map(|(k, v)| (k.as_str(), *v)).collect();

    let Some(out) = rs_compute(&path, &map) else { return Ok(None); };
    let d = PyDict::new_bound(py);
    d.set_item("bid", out.bid)?;
    d.set_item("ask", out.ask)?;
    d.set_item("mid", out.mid)?;
    d.set_item("conf", out.conf)?;
    Ok(Some(d.unbind().into_any()))
}
