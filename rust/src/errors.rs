//! Plan-tier error types for the NX Rates Rust SDK.
//!
//! Mirrors the server-side wire shape defined in
//! `nx-rates/core/src/server/plan_errors.rs` + documented in
//! `docs/api-plans.md` (§ "Error codes and SDK handling"). When the server
//! responds with a 4xx whose JSON body has `error == "PLAN_LIMIT_EXCEEDED"`,
//! [`PlanLimitError::from_response_body`] parses it into a typed error so
//! callers can branch on [`PlanErrorCode`] instead of regexing English
//! `message` strings.
//!
//! Stable wire identifiers — never rename without bumping the SDK majors.
//!
//! # Example
//!
//! ```no_run
//! use nxr_sdk::errors::{PlanLimitError, PlanErrorCode};
//!
//! # async fn run() -> anyhow::Result<()> {
//! # let c: nxr_sdk::client::NxrClient = todo!();
//! match c.idx("BTC/USDT", &Default::default()).await {
//!     Ok(rs) => println!("{} records", rs.len()),
//!     Err(e) => {
//!         if let Some(plan_err) = e.downcast_ref::<PlanLimitError>() {
//!             eprintln!("{}: {}", plan_err.code.as_str(), plan_err.message);
//!             if plan_err.is_upgrade_needed() {
//!                 eprintln!("Upgrade → {}", plan_err.upgrade_url);
//!             }
//!         } else {
//!             return Err(e);
//!         }
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! Today the `client.rs` REST methods still return `anyhow::Error` directly;
//! once the server-side enforcement lands, the helpers will downcast and
//! surface this type at the boundary. The wire-shape definitions and the
//! parser are landed here first so SDK consumers can pre-code against the
//! taxonomy.

use serde::{Deserialize, Serialize};

/// Top-level discriminant. SDK only treats a body as a plan error when this
/// field equals the constant.
pub const PLAN_ERROR_DISCRIMINANT: &str = "PLAN_LIMIT_EXCEEDED";

/// Stable plan-tier error code taxonomy. Mirrors `PlanErrorCode` in the
/// server core. Wire identifiers (SCREAMING_SNAKE_CASE) emitted via serde.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlanErrorCode {
    /// HTTP rate-limit bucket empty (status 429). Retry with backoff.
    PlanRateLimitHttp,
    /// WebSocket message-rate exceeded (close 4029). Retry with backoff.
    PlanRateLimitWs,
    /// Too many WS feeds for the plan (status 403 / close 4030). Upgrade.
    PlanWsFeedCap,
    /// MITCH / f64 encoding requested on Free (status 406). Upgrade or use JSON.
    PlanEncodingForbidden,
    /// Timeframe below plan floor (status 403). Upgrade or coarsen TF.
    PlanTimeframeForbidden,
    /// `from` older than plan window (status 403). Upgrade or shorten range.
    PlanHistoryForbidden,
    /// Endpoint requires a key (status 401). Provide a key.
    PlanAuthRequired,
    /// `X-NXR-Key` unknown (status 401). Verify the key.
    PlanKeyInvalid,
    /// Key disabled / revoked (status 403). Rotate or contact support.
    PlanKeyRevoked,
}

impl PlanErrorCode {
    /// Stable wire string. Matches serde output.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PlanRateLimitHttp => "PLAN_RATE_LIMIT_HTTP",
            Self::PlanRateLimitWs => "PLAN_RATE_LIMIT_WS",
            Self::PlanWsFeedCap => "PLAN_WS_FEED_CAP",
            Self::PlanEncodingForbidden => "PLAN_ENCODING_FORBIDDEN",
            Self::PlanTimeframeForbidden => "PLAN_TIMEFRAME_FORBIDDEN",
            Self::PlanHistoryForbidden => "PLAN_HISTORY_FORBIDDEN",
            Self::PlanAuthRequired => "PLAN_AUTH_REQUIRED",
            Self::PlanKeyInvalid => "PLAN_KEY_INVALID",
            Self::PlanKeyRevoked => "PLAN_KEY_REVOKED",
        }
    }

    /// True for codes whose resolution is "upgrade plan" rather than "retry".
    pub fn is_upgrade_needed(&self) -> bool {
        matches!(
            self,
            Self::PlanEncodingForbidden
                | Self::PlanTimeframeForbidden
                | Self::PlanHistoryForbidden
                | Self::PlanWsFeedCap
        )
    }

    /// True for rate-limit codes — caller should back off + retry.
    pub fn is_rate_limit(&self) -> bool {
        matches!(self, Self::PlanRateLimitHttp | Self::PlanRateLimitWs)
    }

    /// True for auth-related codes — caller should re-auth, not upgrade.
    pub fn is_auth_error(&self) -> bool {
        matches!(
            self,
            Self::PlanAuthRequired | Self::PlanKeyInvalid | Self::PlanKeyRevoked
        )
    }
}

/// Wire JSON body shape — exact mirror of `PlanErrorBody` in the server.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanLimitErrorBody {
    /// Always `"PLAN_LIMIT_EXCEEDED"` for plan errors.
    pub error: String,
    pub code: PlanErrorCode,
    pub message: String,
    pub plan: String,
    pub limit_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_value: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<u64>,
    pub upgrade_url: String,
}

/// Typed plan-tier error returned by the SDK on any 4xx whose JSON body
/// matches the [`PlanLimitErrorBody`] wire shape.
///
/// Implements [`std::error::Error`] + [`Display`] so it composes with
/// `anyhow` / `?` / `thiserror`-using callers without further wiring.
#[derive(Clone, Debug)]
pub struct PlanLimitError {
    pub code: PlanErrorCode,
    pub plan: String,
    pub limit_name: String,
    pub limit_value: Option<u64>,
    pub requested: Option<u64>,
    pub upgrade_url: String,
    pub http_status: u16,
    pub message: String,
}

impl PlanLimitError {
    /// HTTP statuses we even attempt to parse as plan errors.
    pub const CANDIDATE_STATUSES: &'static [u16] = &[401, 403, 406, 429];

    /// Heuristic: a body is a plan error when discriminant + known code +
    /// required string fields all match. Defensive against partial server
    /// rollouts where the same URL may emit a generic 4xx during a deploy.
    pub fn is_plan_limit_body(body: &serde_json::Value) -> bool {
        let Some(err) = body.get("error").and_then(|v| v.as_str()) else {
            return false;
        };
        if err != PLAN_ERROR_DISCRIMINANT {
            return false;
        }
        // `code` must serde-decode into one of the known variants.
        let Some(code_str) = body.get("code").and_then(|v| v.as_str()) else {
            return false;
        };
        if serde_json::from_str::<PlanErrorCode>(&format!("\"{code_str}\"")).is_err() {
            return false;
        }
        for f in ["message", "plan", "limit_name", "upgrade_url"] {
            if body.get(f).and_then(|v| v.as_str()).is_none() {
                return false;
            }
        }
        true
    }

    /// Construct from a parsed [`PlanLimitErrorBody`] + the observed HTTP status.
    pub fn from_body(body: PlanLimitErrorBody, http_status: u16) -> Self {
        Self {
            code: body.code,
            plan: body.plan,
            limit_name: body.limit_name,
            limit_value: body.limit_value,
            requested: body.requested,
            upgrade_url: body.upgrade_url,
            http_status,
            message: body.message,
        }
    }

    /// Try to parse a [`PlanLimitError`] from raw response bytes + status.
    ///
    /// Returns `None` (so the caller falls through to a generic HTTP error)
    /// when:
    ///  - status is outside `{401, 403, 406, 429}`, OR
    ///  - body is not valid JSON, OR
    ///  - the JSON does not match the plan-error wire shape.
    pub fn from_response_body(http_status: u16, body: &[u8]) -> Option<Self> {
        if !Self::CANDIDATE_STATUSES.contains(&http_status) {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(body).ok()?;
        if !Self::is_plan_limit_body(&v) {
            return None;
        }
        let parsed: PlanLimitErrorBody = serde_json::from_value(v).ok()?;
        Some(Self::from_body(parsed, http_status))
    }

    /// Construct from an already-decoded JSON value (e.g. a WS close-frame
    /// payload). Returns `None` if the body does not match the wire shape.
    pub fn from_json_value(body: &serde_json::Value, http_status: u16) -> Option<Self> {
        if !Self::is_plan_limit_body(body) {
            return None;
        }
        let parsed: PlanLimitErrorBody = serde_json::from_value(body.clone()).ok()?;
        Some(Self::from_body(parsed, http_status))
    }

    /// True for codes whose resolution is "upgrade plan".
    pub fn is_upgrade_needed(&self) -> bool {
        self.code.is_upgrade_needed()
    }

    /// True for rate-limit codes.
    pub fn is_rate_limit(&self) -> bool {
        self.code.is_rate_limit()
    }

    /// True for auth-related codes.
    pub fn is_auth_error(&self) -> bool {
        self.code.is_auth_error()
    }
}

impl std::fmt::Display for PlanLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)?;
        if let Some(v) = self.limit_value {
            write!(f, " (plan='{}', limit_name='{}', limit_value={}", self.plan, self.limit_name, v)?;
        } else {
            write!(f, " (plan='{}', limit_name='{}'", self.plan, self.limit_name)?;
        }
        if let Some(r) = self.requested {
            write!(f, ", requested={}", r)?;
        }
        write!(f, ", http_status={})", self.http_status)
    }
}

impl std::error::Error for PlanLimitError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_body() -> serde_json::Value {
        serde_json::json!({
            "error": "PLAN_LIMIT_EXCEEDED",
            "code": "PLAN_WS_FEED_CAP",
            "message": "Plan 'starter' allows max 10 WS feeds; you requested 25.",
            "plan": "starter",
            "limit_name": "ws_feed_cap",
            "limit_value": 10,
            "requested": 25,
            "upgrade_url": "https://api.nxrates.com/v1/plans/upgrade"
        })
    }

    #[test]
    fn parses_canonical_body() {
        let raw = serde_json::to_vec(&sample_body()).unwrap();
        let e = PlanLimitError::from_response_body(403, &raw).expect("parsed");
        assert_eq!(e.code, PlanErrorCode::PlanWsFeedCap);
        assert_eq!(e.plan, "starter");
        assert_eq!(e.limit_name, "ws_feed_cap");
        assert_eq!(e.limit_value, Some(10));
        assert_eq!(e.requested, Some(25));
        assert_eq!(e.http_status, 403);
        assert!(e.is_upgrade_needed());
        assert!(!e.is_rate_limit());
        assert!(!e.is_auth_error());
    }

    #[test]
    fn rejects_wrong_status() {
        let raw = serde_json::to_vec(&sample_body()).unwrap();
        assert!(PlanLimitError::from_response_body(200, &raw).is_none());
        assert!(PlanLimitError::from_response_body(500, &raw).is_none());
    }

    #[test]
    fn rejects_garbage_body() {
        let bad = b"not json at all";
        assert!(PlanLimitError::from_response_body(403, bad).is_none());
        let mismatched = b"{\"error\":\"other\"}";
        assert!(PlanLimitError::from_response_body(403, mismatched).is_none());
    }

    #[test]
    fn rejects_unknown_code() {
        let mut v = sample_body();
        v["code"] = serde_json::Value::String("PLAN_NOT_A_REAL_CODE".into());
        let raw = serde_json::to_vec(&v).unwrap();
        assert!(PlanLimitError::from_response_body(403, &raw).is_none());
    }

    #[test]
    fn display_renders_full_context() {
        let raw = serde_json::to_vec(&sample_body()).unwrap();
        let e = PlanLimitError::from_response_body(403, &raw).unwrap();
        let s = format!("{}", e);
        assert!(s.contains("PLAN_WS_FEED_CAP"));
        assert!(s.contains("plan='starter'"));
        assert!(s.contains("limit_value=10"));
        assert!(s.contains("requested=25"));
        assert!(s.contains("http_status=403"));
    }

    #[test]
    fn code_as_str_matches_serde() {
        for code in [
            PlanErrorCode::PlanRateLimitHttp,
            PlanErrorCode::PlanRateLimitWs,
            PlanErrorCode::PlanWsFeedCap,
            PlanErrorCode::PlanEncodingForbidden,
            PlanErrorCode::PlanTimeframeForbidden,
            PlanErrorCode::PlanHistoryForbidden,
            PlanErrorCode::PlanAuthRequired,
            PlanErrorCode::PlanKeyInvalid,
            PlanErrorCode::PlanKeyRevoked,
        ] {
            let wire = serde_json::to_string(&code).unwrap();
            assert_eq!(wire.trim_matches('"'), code.as_str());
        }
    }

    #[test]
    fn helpers_categorize_codes() {
        assert!(PlanErrorCode::PlanAuthRequired.is_auth_error());
        assert!(PlanErrorCode::PlanRateLimitHttp.is_rate_limit());
        assert!(PlanErrorCode::PlanWsFeedCap.is_upgrade_needed());
        assert!(!PlanErrorCode::PlanRateLimitHttp.is_upgrade_needed());
    }
}
