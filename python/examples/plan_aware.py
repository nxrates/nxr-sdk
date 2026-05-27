#!/usr/bin/env python3
"""NXR Python SDK — plan-aware error handling demo.

Demonstrates the typed PlanLimitError surface from ``nxr_sdk``:
  1. Reads NXR_API_KEY from env (Free / Starter+ depending on key)
  2. Catches PlanLimitError specifically — not generic Exception
  3. Pretty-prints code / plan / limit / requested with an upgrade CTA
  4. Falls back gracefully (e.g. MITCH → JSON on Free, tf=10 → tf=60)

Run::

    cd sdk/python && maturin develop --release
    NXR_API_KEY=<key> python examples/plan_aware.py
    # Or anonymous (Free tier):
    python examples/plan_aware.py
"""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request

import nxr_sdk
from nxr_sdk.errors import (
    PLAN_ENCODING_FORBIDDEN,
    PLAN_TIMEFRAME_FORBIDDEN,
    PlanLimitError,
    parse_plan_limit_error,
)

BASE_URL = os.environ.get("NXR_BASE_URL", "https://api.nxrates.com")
API_KEY = os.environ.get("NXR_API_KEY")  # None = Free tier
SEP = "=" * 72

print(SEP)
print(f"NXR Python SDK · plan-aware demo · key={'set' if API_KEY else 'NONE (Free)'}")
print(SEP)


def report_plan_error(e: PlanLimitError, scenario: str) -> None:
    """Pretty-print a PlanLimitError with all wire fields + an actionable CTA."""
    print(f"\n[!] PlanLimitError in scenario: {scenario}")
    print(f"    code        = {e.code}")
    print(f"    plan        = {e.plan}")
    print(f"    limit_name  = {e.limit_name}")
    if e.limit_value is not None:
        print(f"    limit_value = {e.limit_value}")
    if e.requested is not None:
        print(f"    requested   = {e.requested}")
    print(f"    http_status = {e.http_status}")
    print(f"    message     = {e.message}")
    if e.is_upgrade_needed():
        print(f"    -> action   : upgrade plan → {e.upgrade_url}")
    elif e.is_rate_limit():
        print(f"    -> action   : back off + retry (rate-limit)")
    elif e.is_auth_error():
        print(f"    -> action   : verify API key (auth error)")


def _http_get(path: str, accept: str) -> tuple[int, bytes, str]:
    """Issue a raw HTTP GET with the given Accept header. Returns
    (status, body_bytes, content_type). On HTTPError, surfaces the response
    body so the plan-error parser can inspect it.
    """
    headers = {"Accept": accept, "User-Agent": "nxr-sdk-python/plan-aware"}
    if API_KEY:
        headers["X-NXR-Key"] = API_KEY
    req = urllib.request.Request(f"{BASE_URL}{path}", headers=headers)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read(), r.headers.get("Content-Type", "")
    except urllib.error.HTTPError as exc:
        body = exc.read() if exc.fp is not None else b""
        ct = exc.headers.get("Content-Type", "") if exc.headers else ""
        return exc.code, body, ct


# ── Scenario 1: /v1/tickers (cheap, JSON, allowed on every plan) ────────────

t0 = time.perf_counter()
status, body, ct = _http_get("/v1/tickers", accept="application/json")
dt_ms = (time.perf_counter() - t0) * 1000
plan_err = parse_plan_limit_error(status, body, content_type=ct)
if plan_err:
    report_plan_error(plan_err, "tickers (unexpected)")
else:
    n = len(json.loads(body))
    print(f"\n[1] /v1/tickers          → {n} mids in {dt_ms:.1f}ms (no plan limit hit)")

# ── Scenario 2: MITCH binary on Free → PLAN_ENCODING_FORBIDDEN ──────────────

print("\n[2] /v1/idx/BTC-USDT (MITCH binary)")
status, body, ct = _http_get(
    "/v1/idx/BTC-USDT?limit=100", accept="application/octet-stream"
)
plan_err = parse_plan_limit_error(status, body, content_type=ct)
if plan_err and plan_err.code == PLAN_ENCODING_FORBIDDEN:
    report_plan_error(plan_err, "MITCH binary on Free")
    # Graceful fallback — JSON path is allowed everywhere.
    print("    → falling back to JSON /v1/ohlc (allowed on Free)")
    s2, b2, _ct2 = _http_get(
        "/v1/ohlc/BTC-USDT?tf=60&limit=100", accept="application/json"
    )
    if 200 <= s2 < 300:
        print(f"    ok · {len(json.loads(b2))} OHLC bars via JSON fallback")
    else:
        print(f"    fallback also failed: HTTP {s2}")
elif plan_err:
    report_plan_error(plan_err, "MITCH binary")
elif 200 <= status < 300:
    arr = nxr_sdk.decode_idx_bytes(body)
    print(f"    ok · {len(arr)} IndexRecords decoded (binary path)")
else:
    print(f"    HTTP {status} (non-plan): {body[:120]!r}")

# ── Scenario 3: tf=10 → PLAN_TIMEFRAME_FORBIDDEN on Free/Starter ────────────

print("\n[3] /v1/ohlc tf=10 (Pro-only)")
status, body, ct = _http_get(
    "/v1/ohlc/BTC-USDT?tf=10&limit=10", accept="application/json"
)
plan_err = parse_plan_limit_error(status, body, content_type=ct)
if plan_err and plan_err.code == PLAN_TIMEFRAME_FORBIDDEN:
    report_plan_error(plan_err, "tf=10s requested")
    print("    → falling back to tf=60 (allowed on Free)")
    s2, b2, _ct2 = _http_get(
        "/v1/ohlc/BTC-USDT?tf=60&limit=10", accept="application/json"
    )
    if 200 <= s2 < 300:
        print(f"    ok · {len(json.loads(b2))} bars at tf=60 via fallback")
elif plan_err:
    report_plan_error(plan_err, "tf=10 request")
elif 200 <= status < 300:
    print(f"    ok · {len(json.loads(body))} bars at tf=10 (Pro+ key detected)")
else:
    print(f"    HTTP {status} (non-plan): {body[:120]!r}")

# ── Scenario 4: NxrClient high-level path with broad try/except ─────────────
# Shows how a downstream app wraps the typed exception around the high-level
# API. Note: today's pyo3 Client raises a generic Exception; once the server
# emits PLAN_LIMIT_EXCEEDED bodies, the SDK will downcast at the boundary
# (see docs/api-plans.md §"Error codes and SDK handling").

print("\n[4] NxrClient.history(...) with typed-exception wrapper")
nxr = nxr_sdk.NxrClient(base_url=BASE_URL)
try:
    # Today this path doesn't raise PlanLimitError — added for the post-
    # enforcement future. The pattern is the contract the SDKs commit to.
    arr = nxr.fetch_idx("BTC-USDT", limit=10)
    print(f"    ok · {len(arr)} IndexRecords (binary path, key-permitted)")
except PlanLimitError as e:
    report_plan_error(e, "fetch_idx via NxrClient")
    print("    → falling back to fetch_ohlc with tf=60")
    arr = nxr.fetch_ohlc("BTC-USDT", 60, limit=10)
    print(f"    ok · {len(arr)} OHLC bars via fallback")
except Exception as e:
    # Inspect string of the underlying error in case the server already
    # returned a plan-error body and the pyo3 layer hasn't been wired yet.
    msg = str(e)
    if "PLAN_LIMIT_EXCEEDED" in msg:
        # Best-effort parse from the error message.
        try:
            start = msg.find("{")
            end = msg.rfind("}") + 1
            body_dict = json.loads(msg[start:end])
            wrapped = parse_plan_limit_error(403, body_dict, "application/json")
            if wrapped:
                report_plan_error(wrapped, "fetch_idx (parsed from string)")
        except Exception:
            print(f"    raw error (could not extract): {e}")
    else:
        print(f"    generic error (not plan-related): {e}")

print("\n" + SEP)
print("Plan-aware demo complete.")
print(SEP)
