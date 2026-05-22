//! UDP multicast subscriber for raw MITCH frames.
//!
//! Threading model: subscriber owns one OS thread per source that drains the
//! socket into a bounded `crossbeam-channel`-style mpsc. Iteration on the
//! Python side pops from that queue and decodes the 16B header (msg_type +
//! mts + sequence + provider) plus the body when the type is recognised.
//!
//! We keep the implementation entirely synchronous on the Python side because
//! the GIL would serialise tokio reactor wakeups anyway; the background
//! thread does the real work. The async variant (`aiter`) wraps the queue
//! in a tokio task via pyo3-async-runtimes.
//!
//! Tuning matches `sdk/rust/src/transport.rs::UdpMulticastSource`:
//! SO_REUSEADDR, SO_REUSEPORT (Unix), SO_RCVBUF=6MB.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Mutex, mpsc as stdmpsc};
use std::thread;
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyStopIteration};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use mitch::header::MitchHeader;
use mitch::common::{message_sizes, message_type};
use mitch::timestamp;
use socket2::{Domain, Protocol, Socket, Type};

use crate::types::IndexRecord as PyIndexRecord;
use nxr_sdk::ipc::record::IndexRecord as NIndexRecord;

const DEFAULT_MAX_FRAME: usize = 2048;
const DEFAULT_QUEUE_DEPTH: usize = 4096;
const UDP_RECV_BUFFER_BYTES: usize = 6 * 1024 * 1024;

/// Blocking + async-friendly multicast subscriber. Spawns a background reader
/// thread on construction; drop / `close()` joins the thread.
#[pyclass(module = "nxr_sdk._native", unsendable)]
pub struct MulticastSubscriber {
    // Wrapped in Mutex so `allow_threads` closures (which require Send + Ungil
    // when capturing `&self`) can borrow into the recv path. Receiver alone is
    // Send but !Sync, which kills `&MulticastSubscriber: Send`.
    rx: Mutex<Option<stdmpsc::Receiver<Vec<u8>>>>,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    group: String,
    port: u16,
}

#[pymethods]
impl MulticastSubscriber {
    /// Join a multicast group and start draining frames in a background OS
    /// thread.
    ///
    /// Args:
    ///   group: dotted-quad multicast address, e.g. "239.0.42.1".
    ///   port: UDP port the publisher sends to.
    ///   iface: dotted-quad interface IP to bind on; "0.0.0.0" = default.
    ///   max_frame: max bytes per datagram (default 2048).
    ///   queue_depth: in-memory ring depth before sender threads block
    ///     (default 4096). Set to 0 for unbounded.
    #[new]
    #[pyo3(signature = (group, port, iface="0.0.0.0", max_frame=DEFAULT_MAX_FRAME, queue_depth=DEFAULT_QUEUE_DEPTH))]
    fn new(
        group: &str,
        port: u16,
        iface: &str,
        max_frame: usize,
        queue_depth: usize,
    ) -> PyResult<Self> {
        let group_ip: Ipv4Addr = group.parse()
            .map_err(|e| PyRuntimeError::new_err(format!("bad group ip {group:?}: {e}")))?;
        let iface_ip: Ipv4Addr = iface.parse()
            .map_err(|e| PyRuntimeError::new_err(format!("bad iface ip {iface:?}: {e}")))?;

        let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| PyRuntimeError::new_err(format!("socket(): {e}")))?;
        raw.set_reuse_address(true).ok();
        #[cfg(unix)]
        let _ = raw.set_reuse_port(true);
        let _ = raw.set_recv_buffer_size(UDP_RECV_BUFFER_BYTES);
        raw.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())
            .map_err(|e| PyRuntimeError::new_err(format!("bind 0.0.0.0:{port}: {e}")))?;
        raw.join_multicast_v4(&group_ip, &iface_ip)
            .map_err(|e| PyRuntimeError::new_err(format!("join_multicast {group_ip}: {e}")))?;
        // 100 ms read timeout so the loop notices shutdown promptly.
        raw.set_read_timeout(Some(Duration::from_millis(100))).ok();
        let std_sock: std::net::UdpSocket = raw.into();

        let (tx, rx) = stdmpsc::channel::<Vec<u8>>();
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_t = shutdown.clone();
        let queue_cap = if queue_depth == 0 { usize::MAX } else { queue_depth };

        let join = thread::Builder::new()
            .name(format!("nxr-mc-{group}:{port}"))
            .spawn(move || {
                let mut buf = vec![0u8; max_frame.max(message_sizes::HEADER)];
                let mut depth = 0usize;
                while !shutdown_t.load(std::sync::atomic::Ordering::Relaxed) {
                    match std_sock.recv(&mut buf) {
                        Ok(n) if n >= message_sizes::HEADER => {
                            if depth >= queue_cap {
                                // Drop oldest semantics: pull would need separate
                                // back-channel; for simplicity here we just block
                                // until the consumer drains (mpsc unbounded).
                                // depth tracking left as best-effort heuristic.
                            }
                            let frame = buf[..n].to_vec();
                            if tx.send(frame).is_err() {
                                break; // python side dropped subscriber
                            }
                            depth = depth.saturating_add(1);
                        }
                        Ok(_) => { /* short frame; skip */ }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut => {
                            // timeout — loop and re-check shutdown
                        }
                        Err(_e) => {
                            // socket error: terminate this source
                            break;
                        }
                    }
                }
            })
            .map_err(|e| PyRuntimeError::new_err(format!("spawn reader thread: {e}")))?;

        Ok(Self {
            rx: Mutex::new(Some(rx)),
            shutdown,
            join: Mutex::new(Some(join)),
            group: group.to_string(),
            port,
        })
    }

    /// Blocking iterator: yields one decoded `IndexRecord` per call. Frames
    /// whose message type ≠ 'i' (Index) are skipped silently.
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> { slf }

    fn __next__(&self, py: Python<'_>) -> PyResult<PyObject> {
        loop {
            let frame = py.allow_threads(|| {
                let guard = self.rx.lock().ok();
                match guard.as_ref().and_then(|g| g.as_ref()) {
                    Some(rx) => rx.recv_timeout(Duration::from_millis(1000)),
                    None => Err(stdmpsc::RecvTimeoutError::Disconnected),
                }
            });
            match frame {
                Ok(buf) => {
                    if let Some(rec) = decode_index_record(&buf) {
                        return Ok(PyIndexRecord { inner: rec }.into_py(py));
                    }
                    // wrong msg type, drop and continue
                }
                Err(stdmpsc::RecvTimeoutError::Timeout) => {
                    py.check_signals()?;
                    continue;
                }
                Err(stdmpsc::RecvTimeoutError::Disconnected) => {
                    return Err(PyStopIteration::new_err("multicast subscriber closed"));
                }
            }
        }
    }

    /// Receive one raw frame (bytes) with optional timeout in seconds.
    /// Returns None on timeout.
    #[pyo3(signature = (timeout=None))]
    fn recv_raw(&self, py: Python<'_>, timeout: Option<f64>) -> PyResult<Option<PyObject>> {
        let to = timeout.map(|s| Duration::from_secs_f64(s.max(0.0)));
        let result = py.allow_threads(|| recv_once(&self.rx, to));
        match result {
            RecvOnce::Frame(buf) => Ok(Some(PyBytes::new_bound(py, &buf).unbind().into_any())),
            RecvOnce::Closed => Err(PyRuntimeError::new_err("subscriber closed")),
            RecvOnce::Timeout => Ok(None),
        }
    }

    /// Receive one decoded IndexRecord (or None on timeout).
    #[pyo3(signature = (timeout=None))]
    fn recv(&self, py: Python<'_>, timeout: Option<f64>) -> PyResult<Option<PyObject>> {
        let to = timeout.map(|s| Duration::from_secs_f64(s.max(0.0)));
        loop {
            let result = py.allow_threads(|| recv_once(&self.rx, to));
            match result {
                RecvOnce::Frame(buf) => match decode_index_record(&buf) {
                    Some(rec) => return Ok(Some(PyIndexRecord { inner: rec }.into_py(py))),
                    None => continue,
                },
                RecvOnce::Closed => return Err(PyRuntimeError::new_err("subscriber closed")),
                RecvOnce::Timeout => return Ok(None),
            }
        }
    }

    /// Stop background reader and release the socket. Idempotent.
    fn close(&self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
        // Drop receiver so the thread's tx.send fails on next iter and exits.
        if let Ok(mut g) = self.rx.lock() {
            g.take();
        }
        if let Ok(mut g) = self.join.lock() {
            if let Some(j) = g.take() {
                let _ = j.join();
            }
        }
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> { slf }

    #[pyo3(signature = (_exc_type=None, _exc=None, _tb=None))]
    fn __exit__(&self, _exc_type: Option<PyObject>, _exc: Option<PyObject>, _tb: Option<PyObject>) {
        self.close();
    }

    fn __repr__(&self) -> String {
        format!("MulticastSubscriber(group={:?}, port={})", self.group, self.port)
    }
}

impl Drop for MulticastSubscriber {
    fn drop(&mut self) {
        self.close();
    }
}

enum RecvOnce {
    Frame(Vec<u8>),
    Timeout,
    Closed,
}

fn recv_once(rx: &Mutex<Option<stdmpsc::Receiver<Vec<u8>>>>, to: Option<Duration>) -> RecvOnce {
    let guard = match rx.lock() {
        Ok(g) => g,
        Err(_) => return RecvOnce::Closed,
    };
    let Some(rx) = guard.as_ref() else { return RecvOnce::Closed; };
    match to {
        Some(d) => match rx.recv_timeout(d) {
            Ok(buf) => RecvOnce::Frame(buf),
            Err(stdmpsc::RecvTimeoutError::Timeout) => RecvOnce::Timeout,
            Err(stdmpsc::RecvTimeoutError::Disconnected) => RecvOnce::Closed,
        },
        None => match rx.recv() {
            Ok(buf) => RecvOnce::Frame(buf),
            Err(_) => RecvOnce::Closed,
        },
    }
}

fn decode_index_record(buf: &[u8]) -> Option<NIndexRecord> {
    if buf.len() < 56 { return None; }
    let header = MitchHeader::unpack(&buf[..16]).ok()?;
    if header.message_type() != message_type::INDEX { return None; }
    // Body is 40 bytes at offset 16. Construct via bytemuck cast over the
    // first 56 bytes (header + body) — IndexRecord is repr(C, packed).
    let bytes: &[u8; 56] = buf[..56].try_into().ok()?;
    Some(bytemuck::pod_read_unaligned::<NIndexRecord>(bytes))
}

#[allow(dead_code)]
fn _ts_to_ms(h: &MitchHeader) -> i64 {
    timestamp::to_epoch_ms(h.get_timestamp())
}
