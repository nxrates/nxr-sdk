/**
 * Plan-tier error types for the NX Rates TypeScript SDK.
 *
 * Mirrors the server-side wire shape defined in
 * `core/src/server/plan_errors.rs` + documented in `docs/api-plans.md`
 * (§ "Error codes and SDK handling"). When the server responds with a
 * 4xx whose JSON body has `error: "PLAN_LIMIT_EXCEEDED"`, parse it via
 * {@link parsePlanLimitError} and throw {@link PlanLimitError} so callers
 * can branch on `code` instead of regexing English message strings.
 *
 * Stable wire identifiers — do not rename without bumping the SDK major.
 *
 * @example
 * ```ts
 * import { NxrClient, PlanLimitError } from '@nxrates/sdk';
 *
 * try {
 *   const sub = nxr.subscribe(bigList, onTick);
 * } catch (e) {
 *   if (e instanceof PlanLimitError) {
 *     console.error(`${e.code}: ${e.message}`);
 *     if (e.isUpgradeNeeded()) console.error(`Upgrade → ${e.upgradeUrl}`);
 *   } else throw e;
 * }
 * ```
 */

/** Stable top-level discriminant the SDK matches on. */
export const PLAN_ERROR_DISCRIMINANT = 'PLAN_LIMIT_EXCEEDED';

/**
 * Plan-tier error code taxonomy. Stable wire strings. Mirrors
 * {@link https://github.com/nxrates/nxr/blob/main/core/src/server/plan_errors.rs PlanErrorCode}.
 */
export type PlanErrorCode =
  /** HTTP rate-limit bucket empty. Status 429. Retry with backoff. */
  | 'PLAN_RATE_LIMIT_HTTP'
  /** WS message rate exceeded. WS close 4029. Retry with backoff. */
  | 'PLAN_RATE_LIMIT_WS'
  /** Too many WS feeds for the plan. Status 403 / close 4030. Upgrade. */
  | 'PLAN_WS_FEED_CAP'
  /** MITCH/f64 encoding requested on Free. Status 406. Upgrade or use JSON. */
  | 'PLAN_ENCODING_FORBIDDEN'
  /** Timeframe below the plan floor. Status 403. Upgrade or coarsen TF. */
  | 'PLAN_TIMEFRAME_FORBIDDEN'
  /** `from` older than the plan window. Status 403. Upgrade or shorten range. */
  | 'PLAN_HISTORY_FORBIDDEN'
  /** Endpoint requires a key. Status 401. Provide a key. */
  | 'PLAN_AUTH_REQUIRED'
  /** `X-NXR-Key` header unknown. Status 401. Verify the key. */
  | 'PLAN_KEY_INVALID'
  /** Key disabled / revoked. Status 403. Rotate or contact support. */
  | 'PLAN_KEY_REVOKED';

/** Codes that signal an upgrade-path resolution rather than a retry. */
const UPGRADE_NEEDED_CODES: ReadonlySet<PlanErrorCode> = new Set([
  'PLAN_ENCODING_FORBIDDEN',
  'PLAN_TIMEFRAME_FORBIDDEN',
  'PLAN_HISTORY_FORBIDDEN',
  'PLAN_WS_FEED_CAP',
]);

/** Wire JSON body shape — exact mirror of `PlanErrorBody` in Rust. */
export interface PlanLimitErrorBody {
  /** Always `"PLAN_LIMIT_EXCEEDED"` for plan errors. */
  error: string;
  code: PlanErrorCode;
  message: string;
  plan: string;
  limit_name: string;
  /** Numeric limit. Absent for qualitative codes (e.g. encoding). */
  limit_value?: number;
  /** What the client asked for. Absent when not meaningful. */
  requested?: number;
  upgrade_url: string;
}

/**
 * Typed plan-tier error. Thrown by the SDK on any 4xx whose body parses
 * as a `PlanLimitErrorBody`.
 *
 * Subclass of the standard `Error` so generic catch-all handlers still work;
 * `instanceof PlanLimitError` is the recommended discriminator.
 */
export class PlanLimitError extends Error {
  public readonly code: PlanErrorCode;
  public readonly plan: string;
  public readonly limitName: string;
  public readonly limitValue?: number;
  public readonly requested?: number;
  public readonly upgradeUrl: string;
  public readonly httpStatus: number;
  /** Raw body for power users / forward-compatibility. */
  public readonly raw: PlanLimitErrorBody;

  constructor(body: PlanLimitErrorBody, httpStatus: number) {
    super(body.message);
    this.name = 'PlanLimitError';
    this.code = body.code;
    this.plan = body.plan;
    this.limitName = body.limit_name;
    this.limitValue = body.limit_value;
    this.requested = body.requested;
    this.upgradeUrl = body.upgrade_url;
    this.httpStatus = httpStatus;
    this.raw = body;
    // V8 stack-trace hygiene.
    if (typeof (Error as unknown as { captureStackTrace?: unknown }).captureStackTrace === 'function') {
      (Error as unknown as { captureStackTrace: (t: object, c?: unknown) => void }).captureStackTrace(
        this,
        PlanLimitError,
      );
    }
  }

  /**
   * True for codes whose resolution is "upgrade to a higher plan" rather
   * than "retry after backoff" or "fix the request".
   */
  isUpgradeNeeded(): boolean {
    return UPGRADE_NEEDED_CODES.has(this.code);
  }

  /** True for rate-limit codes — caller should back off + retry. */
  isRateLimit(): boolean {
    return this.code === 'PLAN_RATE_LIMIT_HTTP' || this.code === 'PLAN_RATE_LIMIT_WS';
  }

  /** True for auth-related codes — caller should re-auth, not upgrade. */
  isAuthError(): boolean {
    return (
      this.code === 'PLAN_AUTH_REQUIRED' ||
      this.code === 'PLAN_KEY_INVALID' ||
      this.code === 'PLAN_KEY_REVOKED'
    );
  }
}

/**
 * Heuristic: a value is a plan-error body when it has the right discriminant
 * + a known code. Defensive against partial server rollouts where the same
 * URL may return a generic 4xx during a deploy.
 */
function isPlanLimitBody(v: unknown): v is PlanLimitErrorBody {
  if (typeof v !== 'object' || v === null) return false;
  const o = v as Record<string, unknown>;
  if (o.error !== PLAN_ERROR_DISCRIMINANT) return false;
  if (typeof o.code !== 'string') return false;
  if (typeof o.message !== 'string') return false;
  if (typeof o.plan !== 'string') return false;
  if (typeof o.limit_name !== 'string') return false;
  if (typeof o.upgrade_url !== 'string') return false;
  return true;
}

/**
 * Parse a {@link PlanLimitError} from an HTTP {@link Response}.
 *
 * Behaviour:
 *  - 401 / 403 / 406 / 429 → try to parse the body as `PlanLimitErrorBody`.
 *    On match, returns the typed error. On miss, returns `null` so the caller
 *    can fall through to a generic HTTPError.
 *  - Any other status returns `null` immediately.
 *  - Consumes the response body (clones internally so subsequent reads in the
 *    caller still work).
 *
 * Safe to call on every 4xx without a discriminator — it short-circuits on
 * non-JSON content types.
 */
export async function parsePlanLimitError(res: Response): Promise<PlanLimitError | null> {
  const candidateStatuses = new Set([401, 403, 406, 429]);
  if (!candidateStatuses.has(res.status)) return null;
  const ct = res.headers.get('content-type') ?? '';
  if (!ct.includes('json')) return null;
  // Clone so the caller can still read the body for fallback logging.
  let body: unknown;
  try {
    body = await res.clone().json();
  } catch {
    return null;
  }
  if (!isPlanLimitBody(body)) return null;
  return new PlanLimitError(body, res.status);
}

/**
 * Synchronous variant: parse from an already-decoded JSON value + status.
 * Useful for WS close-frame payloads or non-fetch contexts.
 */
export function planLimitErrorFromJson(body: unknown, httpStatus: number): PlanLimitError | null {
  if (!isPlanLimitBody(body)) return null;
  return new PlanLimitError(body, httpStatus);
}
