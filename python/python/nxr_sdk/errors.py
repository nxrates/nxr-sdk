"""Plan-tier error types for the NX Rates Python SDK.

Mirrors the server-side wire shape defined in
``core/src/server/plan_errors.rs`` + documented in ``docs/api-plans.md``
(§ "Error codes and SDK handling"). When the server responds with a 4xx
whose JSON body has ``error == "PLAN_LIMIT_EXCEEDED"``, parse it via
:func:`parse_plan_limit_error` and raise :class:`PlanLimitError` so callers
can branch on ``code`` instead of regexing English message strings.

Stable wire identifiers — do not rename without bumping the SDK major.

Examples
--------
::

    import nxr_sdk
    from nxr_sdk.errors import PlanLimitError

    try:
        arr = nxr.fetch_idx("BTC-USDT", limit=1000)
    except PlanLimitError as e:
        print(f"{e.code}: {e.message}")
        if e.is_upgrade_needed():
            print(f"Upgrade → {e.upgrade_url}")
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, Iterable, Optional

# ── Constants ───────────────────────────────────────────────────────────────

#: Top-level discriminant. SDK checks for this on every 4xx body.
PLAN_ERROR_DISCRIMINANT = "PLAN_LIMIT_EXCEEDED"

#: Stable code taxonomy. Mirror of `PlanErrorCode` in the Rust core.
PLAN_RATE_LIMIT_HTTP = "PLAN_RATE_LIMIT_HTTP"
PLAN_RATE_LIMIT_WS = "PLAN_RATE_LIMIT_WS"
PLAN_WS_FEED_CAP = "PLAN_WS_FEED_CAP"
PLAN_ENCODING_FORBIDDEN = "PLAN_ENCODING_FORBIDDEN"
PLAN_TIMEFRAME_FORBIDDEN = "PLAN_TIMEFRAME_FORBIDDEN"
PLAN_HISTORY_FORBIDDEN = "PLAN_HISTORY_FORBIDDEN"
PLAN_AUTH_REQUIRED = "PLAN_AUTH_REQUIRED"
PLAN_KEY_INVALID = "PLAN_KEY_INVALID"
PLAN_KEY_REVOKED = "PLAN_KEY_REVOKED"

#: Full known-code set — used to defensively reject partial-rollout responses.
ALL_PLAN_CODES: frozenset[str] = frozenset(
    {
        PLAN_RATE_LIMIT_HTTP,
        PLAN_RATE_LIMIT_WS,
        PLAN_WS_FEED_CAP,
        PLAN_ENCODING_FORBIDDEN,
        PLAN_TIMEFRAME_FORBIDDEN,
        PLAN_HISTORY_FORBIDDEN,
        PLAN_AUTH_REQUIRED,
        PLAN_KEY_INVALID,
        PLAN_KEY_REVOKED,
    }
)

#: Codes whose resolution is "upgrade plan" rather than "retry" or "fix request".
UPGRADE_NEEDED_CODES: frozenset[str] = frozenset(
    {
        PLAN_ENCODING_FORBIDDEN,
        PLAN_TIMEFRAME_FORBIDDEN,
        PLAN_HISTORY_FORBIDDEN,
        PLAN_WS_FEED_CAP,
    }
)

#: HTTP statuses we even attempt to parse as plan errors.
_CANDIDATE_STATUSES: frozenset[int] = frozenset({401, 403, 406, 429})


# ── Exception ───────────────────────────────────────────────────────────────


class PlanLimitError(Exception):
    """Typed plan-tier limit error raised by the SDK on 4xx with the
    ``PLAN_LIMIT_EXCEEDED`` JSON wire shape.

    Subclasses :class:`Exception` so generic catch-all handlers still work;
    use ``isinstance(e, PlanLimitError)`` as the discriminator.

    Attributes
    ----------
    code : str
        Stable wire code (e.g. ``"PLAN_WS_FEED_CAP"``).
    plan : str
        Plan tier name (``"free" | "starter" | "pro" | "enterprise" | "colo"``).
    limit_name : str
        snake_case limit identifier (e.g. ``"ws_feed_cap"``).
    limit_value : int | None
        Numeric limit. ``None`` for qualitative codes (encoding gate).
    requested : int | None
        What the client asked for. ``None`` when not meaningful.
    upgrade_url : str
        CTA URL for upgrade dialogs / docs links.
    http_status : int
        Underlying HTTP status (401/403/406/429).
    message : str
        Human-readable message. Callers MUST NOT pattern-match on it.
    raw : dict[str, Any]
        Full parsed JSON body for power users / forward-compat.
    """

    __slots__ = (
        "code",
        "plan",
        "limit_name",
        "limit_value",
        "requested",
        "upgrade_url",
        "http_status",
        "message",
        "raw",
    )

    def __init__(self, body: dict[str, Any], http_status: int) -> None:
        self.code: str = str(body["code"])
        self.message: str = str(body.get("message", ""))
        self.plan: str = str(body.get("plan", ""))
        self.limit_name: str = str(body.get("limit_name", ""))
        lv = body.get("limit_value")
        self.limit_value: Optional[int] = int(lv) if lv is not None else None
        rq = body.get("requested")
        self.requested: Optional[int] = int(rq) if rq is not None else None
        self.upgrade_url: str = str(body.get("upgrade_url", ""))
        self.http_status: int = int(http_status)
        self.raw: dict[str, Any] = dict(body)
        super().__init__(f"{self.code}: {self.message}")

    # ── Helpers ─────────────────────────────────────────────────────────

    def is_upgrade_needed(self) -> bool:
        """True for codes whose resolution is "upgrade plan"."""
        return self.code in UPGRADE_NEEDED_CODES

    def is_rate_limit(self) -> bool:
        """True for HTTP / WS rate-limit codes — caller should back off + retry."""
        return self.code in (PLAN_RATE_LIMIT_HTTP, PLAN_RATE_LIMIT_WS)

    def is_auth_error(self) -> bool:
        """True for auth-related codes — caller should re-auth, not upgrade."""
        return self.code in (PLAN_AUTH_REQUIRED, PLAN_KEY_INVALID, PLAN_KEY_REVOKED)

    # ── Repr ────────────────────────────────────────────────────────────

    def __repr__(self) -> str:
        return (
            f"PlanLimitError(code={self.code!r}, plan={self.plan!r}, "
            f"limit_name={self.limit_name!r}, limit_value={self.limit_value!r}, "
            f"requested={self.requested!r}, http_status={self.http_status})"
        )


# ── Parsing helpers ─────────────────────────────────────────────────────────


def _is_plan_limit_body(v: Any) -> bool:
    """Heuristic: a value is a plan-error body when it has the right
    discriminant + a *known* code + all required string fields.

    Defensive against partial server rollouts where the same URL might emit a
    generic 4xx during a deploy.
    """
    if not isinstance(v, dict):
        return False
    if v.get("error") != PLAN_ERROR_DISCRIMINANT:
        return False
    code = v.get("code")
    if not isinstance(code, str) or code not in ALL_PLAN_CODES:
        return False
    for f in ("message", "plan", "limit_name", "upgrade_url"):
        if not isinstance(v.get(f), str):
            return False
    return True


def parse_plan_limit_error(
    status: int,
    body: bytes | str | dict[str, Any] | None,
    content_type: Optional[str] = None,
) -> Optional[PlanLimitError]:
    """Parse a :class:`PlanLimitError` from an HTTP response.

    Parameters
    ----------
    status : int
        HTTP status code.
    body : bytes | str | dict | None
        Response body. ``bytes`` / ``str`` are JSON-decoded; ``dict`` is used
        as-is. ``None`` → no error.
    content_type : str | None
        Response ``Content-Type`` header (for the early-exit on non-JSON).

    Returns
    -------
    PlanLimitError | None
        Typed error on match, ``None`` so callers fall through to generic
        HTTP-error handling.

    Notes
    -----
    Returns ``None`` for any status outside ``{401, 403, 406, 429}`` so callers
    can blanket-call this without status-gating.
    """
    if status not in _CANDIDATE_STATUSES:
        return None
    if content_type is not None and "json" not in content_type.lower():
        return None
    parsed: Any
    if body is None:
        return None
    if isinstance(body, dict):
        parsed = body
    else:
        if isinstance(body, bytes):
            try:
                text = body.decode("utf-8")
            except UnicodeDecodeError:
                return None
        else:
            text = body
        try:
            parsed = json.loads(text)
        except (ValueError, json.JSONDecodeError):
            return None
    if not _is_plan_limit_body(parsed):
        return None
    return PlanLimitError(parsed, status)


def plan_limit_error_from_json(body: Any, http_status: int) -> Optional[PlanLimitError]:
    """Construct a :class:`PlanLimitError` from an already-decoded JSON value.

    Useful for WS close-frame payloads or non-HTTP contexts.

    Returns ``None`` if the body does not match the plan-error wire shape.
    """
    if not _is_plan_limit_body(body):
        return None
    return PlanLimitError(body, http_status)


__all__ = [
    "PLAN_ERROR_DISCRIMINANT",
    "PLAN_RATE_LIMIT_HTTP",
    "PLAN_RATE_LIMIT_WS",
    "PLAN_WS_FEED_CAP",
    "PLAN_ENCODING_FORBIDDEN",
    "PLAN_TIMEFRAME_FORBIDDEN",
    "PLAN_HISTORY_FORBIDDEN",
    "PLAN_AUTH_REQUIRED",
    "PLAN_KEY_INVALID",
    "PLAN_KEY_REVOKED",
    "ALL_PLAN_CODES",
    "UPGRADE_NEEDED_CODES",
    "PlanLimitError",
    "parse_plan_limit_error",
    "plan_limit_error_from_json",
]
