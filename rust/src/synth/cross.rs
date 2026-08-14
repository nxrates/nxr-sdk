//! The cross resolver: ONE graph over MITCH asset ids.
//!
//! Every live primary ticker is an undirected edge between its two packed
//! MITCH asset ids, usable in either direction (inverted, exponent flipped).
//! Crossing any pair is then a shortest-path query on that graph, so nothing
//! here is anchored to USD, USDT or any other pivot, and nothing is gated on
//! which forwarder observed the leg: a leg is a leg whether it arrived from
//! cTrader, Pyth, IBKR or a CEX.
//!
//! Symbol TEXT is resolved to asset ids once, at the edge ([`Self::route_sym`]);
//! all graph work is on ids. String forms are a presentation concern.
//!
//! ## Leg algebra
//!
//! A ticker `A/B` prices one A in units of B. A route `X → a₁ → … → Y` is the
//! signed product of its steps, so the result prices one X in units of Y:
//!
//! ```text
//! step u → v over ticker u/v  ⇒  exp = +1   (v per u, as published)
//! step u → v over ticker v/u  ⇒  exp = -1   (invert: 1 / (u per v))
//! ```
//!
//! ## Route preference
//!
//! Fewest legs first (each hop compounds spread, staleness and uncertainty),
//! then the DEEPEST route by its THINNEST leg: a composition is only as
//! trustworthy as its weakest hop, and summing depth lets one deep leg mask a
//! leg with no book behind it. Remaining ties break on the leg symbols, which
//! keeps a volume-blind caller (no weights file yet) deterministic and, because
//! `EUR/USD` sorts before `EUR/USDT`, keeps the deep USD pivot that the retired
//! per-symbol `SYNTH_PATHS` pins used to hard-code.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use mitch::ticker::{TickerId, pack_asset};

use super::paths::{Leg, normalize_to_slash};
use super::tick::{LegTick, SynthTick, compose_legs};

/// Packed MITCH asset id (`class << 16 | class_id`, see `mitch::pack_asset`).
pub type AssetId = u32;

/// `(base, quote)` packed asset ids of a MITCH ticker id.
#[inline]
pub fn ticker_assets(ticker_id: u64) -> (AssetId, AssetId) {
    let t = TickerId::from_raw(ticker_id);
    (
        pack_asset(t.base_asset_class(), t.base_asset_id()),
        pack_asset(t.quote_asset_class(), t.quote_asset_id()),
    )
}

/// `(base, quote)` asset ids of a symbol in any accepted text form
/// (`JPY/USD`, `JPY-USD`, `USDJPY`, a MITCH id is the caller's own lookup).
/// `None` when the symbol has no MITCH id at all.
#[inline]
pub fn symbol_assets(sym: &str) -> Option<(AssetId, AssetId)> {
    let id = crate::try_resolve_ticker_id(&normalize_to_slash(&sym.to_uppercase()))?;
    Some(ticker_assets(id))
}

/// Longest route considered.
///
/// 3 covers every shape in production: direct, one pivot (`CHF/JPY` over
/// `USD/CHF` + `USD/JPY`), and an instrument quoted in a non-anchor currency
/// re-quoted through that currency (`GER40/EUR` + `EUR/USD` + `USD/JPY`).
/// ponytail: 3 hops, raising it needs a bound on the frontier expansion below,
/// not just a bigger constant.
pub const MAX_HOPS: usize = 3;

/// One leg of a resolved route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteLeg {
    /// Leg symbol as REGISTERED (presentation only; `ticker_id` is the key).
    pub sym: Arc<str>,
    /// `+1` = leg as published, `-1` = inverted (bid/ask swap on composition).
    pub exp: i8,
    /// MITCH ticker id of the leg.
    pub ticker_id: u64,
}

/// A live quote for one leg, plus the age of the observation behind it.
/// `age_ms: None` = never observed.
///
/// The uncertainty fields live HERE and not on [`LegTick`]: that struct is the
/// shared BTR `compute_synth_tick` input and must not grow.
#[derive(Debug, Clone, Copy)]
pub struct LegQuote {
    pub tick: LegTick,
    pub age_ms: Option<i64>,
    /// `Index::ci` as published (sqrt-compressed µbp of mid). 0 = not carried,
    /// which propagates as UNKNOWN, never as "no uncertainty".
    pub ci_ubp: u16,
    /// The leg's `confidence` byte when it carries `FLAG_CONF_ACTIVE`. `None`
    /// under any other encoding: no active-leg count may then be claimed.
    pub conf_packed: Option<u8>,
    /// `Index::accepted` — corroborating providers behind the leg.
    pub accepted: u8,
}

impl LegQuote {
    /// One leg straight off a live composite. The ONE place a snapshot becomes
    /// a composable leg, so the REST and signing paths cannot decode the same
    /// record differently.
    pub fn from_index(idx: &crate::Index, age_ms: Option<i64>) -> Self {
        Self {
            tick: LegTick {
                bid: idx.bid,
                ask: idx.ask,
                mid: idx.mid(),
                conf: crate::shard::conf_bps(idx.confidence, idx.flags),
            },
            age_ms,
            ci_ubp: idx.ci,
            conf_packed: (idx.flags & crate::shard::FLAG_CONF_ACTIVE != 0)
                .then_some(idx.confidence),
            accepted: idx.accepted,
        }
    }
}

/// A composed quote and its honest provenance.
#[derive(Debug, Clone, Copy)]
pub struct Composed {
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    /// Confidence in bps, the MINIMUM across legs.
    pub conf: u16,
    /// Age of the OLDEST leg observation. `None` when any leg has never been
    /// observed: a composed quote may never read fresher than its inputs, and
    /// an unknown age cannot be claimed as a known one.
    pub age_ms: Option<i64>,
    /// Propagated `Index::ci`. Relative errors add in quadrature under both
    /// multiplication and division, so an inverted leg contributes identically.
    /// 0 when ANY leg published 0: an unknown input cannot compose into a
    /// known output, and understating uncertainty is the attack.
    pub ci_ubp: u16,
    /// Packed `confidence` recombined per axis to the WEAKEST leg (min ticking
    /// count, AND of the fresh-weight verdicts). `None` when any leg lacks the
    /// ACTIVE encoding.
    pub conf_packed: Option<u8>,
    /// Corroboration of the weakest leg. A cross is no broader than its thinnest
    /// hop.
    pub accepted: u8,
}

/// Ordered legs whose signed product is the requested pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub legs: Vec<RouteLeg>,
}

impl Route {
    /// The legs as `(sym, exp)`, for the bar/OHLC composition helpers.
    pub fn as_legs(&self) -> Vec<Leg> {
        self.legs
            .iter()
            .map(|l| Leg::new(l.sym.as_ref(), l.exp))
            .collect()
    }

    /// Compose the route's live quote. `quote` supplies each leg by ticker id;
    /// a leg it cannot supply yields `None` (a clean absence, never a price
    /// invented from the legs that did resolve).
    pub fn compose(&self, quote: impl Fn(u64) -> Option<LegQuote>) -> Option<Composed> {
        let mut ticks: Vec<(i8, LegTick)> = Vec::with_capacity(self.legs.len());
        // `Some(None)` = observed-age unknown on at least one leg.
        let mut age: Option<i64> = None;
        let mut age_known = true;
        // Same algebra as `core::triangulator::triangulate_into`: relative CI in
        // quadrature, `confidence` recombined per axis, `accepted` = weakest leg.
        let mut rel_sq = 0.0_f64;
        let mut ci_known = true;
        let mut active = u32::MAX;
        let mut fresh_ok = true;
        let mut conf_known = true;
        let mut accepted = u8::MAX;
        for l in &self.legs {
            let q = quote(l.ticker_id)?;
            ticks.push((l.exp, q.tick));
            match q.age_ms {
                Some(a) => age = Some(age.map_or(a, |cur| cur.max(a))),
                None => age_known = false,
            }
            if q.ci_ubp == 0 {
                ci_known = false;
            } else {
                let rel = crate::tdwap::decode_ci_ubp(q.ci_ubp) / 1e8;
                rel_sq += rel * rel;
            }
            match q.conf_packed {
                Some(b) => {
                    active = active.min(mitch::index::conf_active_count(b));
                    fresh_ok &= mitch::index::conf_fresh_weight_ok(b);
                }
                None => conf_known = false,
            }
            accepted = accepted.min(q.accepted);
        }
        let SynthTick {
            bid,
            ask,
            mid,
            conf,
        } = compose_legs(&ticks)?;
        Some(Composed {
            bid,
            ask,
            mid,
            conf,
            age_ms: age_known.then_some(age).flatten(),
            ci_ubp: if ci_known && !self.legs.is_empty() {
                crate::tdwap::encode_ci_ubp(rel_sq.sqrt() * 1e8)
            } else {
                0
            },
            conf_packed: (conf_known && !self.legs.is_empty())
                .then(|| mitch::index::conf_pack_active(active, fresh_ok)),
            accepted: if self.legs.is_empty() { 0 } else { accepted },
        })
    }
}

/// One directed traversal of a primary ticker.
#[derive(Debug)]
struct Edge {
    peer: AssetId,
    exp: i8,
    sym: Arc<str>,
    ticker_id: u64,
}

/// Asset graph over the live primary tickers.
///
/// Rebuilt wholesale when the symbol universe changes (a config reload), never
/// per request: it is topology, not market data. Depth is supplied per query so
/// route ranking tracks the current weights without a rebuild.
#[derive(Debug, Default)]
pub struct CrossGraph {
    adj: HashMap<AssetId, Vec<Edge>>,
    /// What a base/quote TOKEN means, learned from the primaries themselves.
    ///
    /// `resolve_ticker` is not a function of the two sides independently: the
    /// quote it detects changes the class filter on the base, so `ETH/USDT`
    /// resolves base CR:5801 (Ethereum) while `ETH/USD` resolves base IP:2101
    /// (indices.csv "Ethereum Index" carries the alias ETH and loads after the
    /// crypto class, so it wins the unfiltered exact-name map). Keyed on strings
    /// that mismatch was invisible; keyed on asset ids it would strand every
    /// such pair, because the index asset has no book anywhere.
    ///
    /// The books are the authority: a token means whichever asset the registered
    /// primaries actually quote, by edge count, so the busiest reading wins.
    token: HashMap<String, AssetId>,
}

impl CrossGraph {
    /// Build from `(symbol, ticker_id)` PRIMARY tickers.
    ///
    /// Only offer tickers with a real book behind them. A composed output used
    /// as an edge is how a cross ends up referencing itself, which is the
    /// self-reference class the retired `derive_legs` pivot guard existed for:
    /// here it is a build-time contract instead of a per-query special case.
    /// Self-pairs (`USDT/USDT`) are dropped: no book backs them and they can
    /// only add a zero-length detour.
    pub fn from_primaries<I, S>(primaries: I) -> Self
    where
        I: IntoIterator<Item = (S, u64)>,
        S: Into<Arc<str>>,
    {
        let mut adj: HashMap<AssetId, Vec<Edge>> = HashMap::new();
        // token -> asset id -> (registered spelling?, edge count). An explicit
        // registered spelling always outranks a CSV alias.
        let mut token_seen: HashMap<String, HashMap<AssetId, (bool, usize)>> = HashMap::new();
        for (sym, ticker_id) in primaries {
            let (b, q) = ticker_assets(ticker_id);
            if b == q {
                continue;
            }
            let sym: Arc<str> = sym.into();
            if let Some((bt, qt)) = crate::split_pair(&normalize_to_slash(&sym.to_uppercase())) {
                for (tok, id) in [(bt, b), (qt, q)] {
                    let e = token_seen
                        .entry(tok.to_string())
                        .or_default()
                        .entry(id)
                        .or_default();
                    e.0 = true;
                    e.1 += 1;
                }
            }
            adj.entry(b).or_default().push(Edge {
                peer: q,
                exp: 1,
                sym: Arc::clone(&sym),
                ticker_id,
            });
            adj.entry(q).or_default().push(Edge {
                peer: b,
                exp: -1,
                sym,
                ticker_id,
            });
        }
        // Deterministic expansion order, so an all-zero-depth query is
        // reproducible rather than hash-order dependent.
        for edges in adj.values_mut() {
            edges.sort_by(|a, b| a.sym.cmp(&b.sym).then(a.exp.cmp(&b.exp)));
        }
        // A delimiter-less primary (`USDJPY`) contributes no token, so fill in
        // from each graph asset's own MITCH name + aliases. Ranked below an
        // explicit registered spelling, and among aliases by edge count, so a
        // shared alias lands on the asset that actually has the books.
        for (&id, edges) in &adj {
            let (class, class_id) = mitch::ticker::unpack_asset(id);
            let Some(asset) = crate::resolve::get_asset_by_id(class, class_id) else {
                continue;
            };
            for key in std::iter::once(asset.name.as_str()).chain(asset.aliases.split('|')) {
                let key = key.trim().to_ascii_uppercase();
                if key.is_empty() {
                    continue;
                }
                let e = token_seen.entry(key).or_default().entry(id).or_default();
                e.1 = e.1.max(edges.len());
            }
        }
        // Registered spelling first, then most-quoted; ties by asset id so the
        // map is stable across rebuilds.
        let token = token_seen
            .into_iter()
            .filter_map(|(tok, ids)| {
                ids.into_iter()
                    .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                    .map(|(id, _)| (tok, id))
            })
            .collect();
        Self { adj, token }
    }

    /// Number of assets reachable in the graph.
    pub fn assets(&self) -> usize {
        self.adj.len()
    }

    /// Resolve `base/quote` (packed asset ids) to a route. `vol` is a leg's 24 h
    /// depth in USD (`0.0` when unknown). `None` = the pair is not composable
    /// from the current primaries, which is the honest answer, not an error.
    pub fn route(&self, base: AssetId, quote: AssetId, vol: &dyn Fn(u64) -> f64) -> Option<Route> {
        // A parity wrap (EURC, QCAD, …) IS its underlying node: identifying the
        // two computes what a 1.0 peg edge would, with no synthetic leg that has
        // no book, no ticker_id and no age behind it (`series_alias::peg_asset`).
        let (base, quote) = (
            crate::series_alias::peg_asset(base),
            crate::series_alias::peg_asset(quote),
        );
        if base == quote {
            // Self-pair: not a cross, and never a listed ticker. `EURC/EUR` lands
            // here too: that price is the peg, not something the books can say.
            return None;
        }
        let mut heap: BinaryHeap<Cand> = BinaryHeap::new();
        heap.push(Cand {
            at: base,
            bottleneck: f64::INFINITY,
            path: vec![base],
            legs: Vec::new(),
        });
        let mut best: HashMap<AssetId, (usize, f64)> = HashMap::new();
        while let Some(c) = heap.pop() {
            if c.at == quote {
                return Some(Route { legs: c.legs });
            }
            if c.legs.len() >= MAX_HOPS {
                continue;
            }
            let label = (c.legs.len(), c.bottleneck);
            if let Some(&seen) = best.get(&c.at)
                && (seen.0 < label.0 || (seen.0 == label.0 && seen.1 >= label.1))
            {
                continue;
            }
            best.insert(c.at, label);
            // On the last admissible hop only the target is worth pushing, so
            // the widest layer collapses to one lookup per edge instead of a
            // full fan-out through a hub asset like USD.
            let last_hop = c.legs.len() + 1 == MAX_HOPS;
            for e in self.adj.get(&c.at).map(Vec::as_slice).unwrap_or(&[]) {
                if (last_hop && e.peer != quote) || c.path.contains(&e.peer) {
                    continue; // out of budget, or a cycle that can only lengthen
                }
                let mut legs = c.legs.clone();
                legs.push(RouteLeg {
                    sym: Arc::clone(&e.sym),
                    exp: e.exp,
                    ticker_id: e.ticker_id,
                });
                let mut path = c.path.clone();
                path.push(e.peer);
                heap.push(Cand {
                    at: e.peer,
                    bottleneck: c.bottleneck.min(vol(e.ticker_id)),
                    path,
                    legs,
                });
            }
        }
        None
    }

    /// [`Self::route`] from a symbol in any accepted text form. Text is
    /// resolved to asset ids here and nowhere deeper.
    pub fn route_sym(&self, sym: &str, vol: &dyn Fn(u64) -> f64) -> Option<Route> {
        let (base, quote) = self.assets_of(sym)?;
        self.route(base, quote, vol)
    }

    /// `(base, quote)` asset ids of a requested pair. Each side is read off the
    /// primaries' own token map first (see [`Self::token`]) and only falls back
    /// to a whole-pair MITCH resolution when a token is not quoted anywhere.
    pub fn assets_of(&self, sym: &str) -> Option<(AssetId, AssetId)> {
        let slash = normalize_to_slash(&sym.to_ascii_uppercase());
        if let Some((b, q)) = crate::split_pair(&slash)
            && let (Some(&bi), Some(&qi)) = (self.token.get(b), self.token.get(q))
        {
            return Some((bi, qi));
        }
        symbol_assets(&slash)
    }
}

/// Dijkstra label: fewest legs, then deepest bottleneck, then smallest leg
/// symbols. Both first two components are monotone along a route (a hop always
/// adds one leg and can only lower the bottleneck), so the greedy order is
/// exact.
#[derive(Debug)]
struct Cand {
    at: AssetId,
    bottleneck: f64,
    path: Vec<AssetId>,
    legs: Vec<RouteLeg>,
}

impl Cand {
    fn syms(&self) -> Vec<&str> {
        self.legs.iter().map(|l| l.sym.as_ref()).collect()
    }
}

impl Ord for Cand {
    /// GREATER = better, so `BinaryHeap` pops the preferred route first.
    fn cmp(&self, o: &Self) -> Ordering {
        o.legs
            .len()
            .cmp(&self.legs.len())
            .then(self.bottleneck.total_cmp(&o.bottleneck))
            .then_with(|| o.syms().cmp(&self.syms()))
    }
}

impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl PartialEq for Cand {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}

impl Eq for Cand {}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(syms: &[&str]) -> CrossGraph {
        CrossGraph::from_primaries(
            syms.iter()
                .map(|s| (*s, crate::resolve_ticker_id(s)))
                .collect::<Vec<_>>(),
        )
    }

    fn blind(_: u64) -> f64 {
        0.0
    }

    fn route(g: &CrossGraph, sym: &str) -> Vec<(String, i8)> {
        g.route_sym(sym, &blind)
            .unwrap_or_else(|| panic!("{sym} must resolve"))
            .legs
            .iter()
            .map(|l| (l.sym.to_string(), l.exp))
            .collect()
    }

    /// The owner's case: neither leg of the requested pair is a listed pair,
    /// and both primaries are stored anchor-first (`USD/X`).
    #[test]
    fn crosses_two_usd_anchored_legs_neither_of_which_is_the_pair() {
        let g = graph(&["USD/CHF", "USD/JPY"]);
        assert_eq!(
            route(&g, "CHF/JPY"),
            vec![("USD/CHF".into(), -1), ("USD/JPY".into(), 1)]
        );
    }

    /// A broker's `EUR/USD` plus an oracle's `USDC/USD`: the retired
    /// `SYNTH_PATHS` pin for exactly this pair, now derived.
    #[test]
    fn crosses_a_broker_leg_against_a_stable_leg() {
        let g = graph(&["EUR/USD", "USDC/USD"]);
        assert_eq!(
            route(&g, "EUR/USDC"),
            vec![("EUR/USD".into(), 1), ("USDC/USD".into(), -1)]
        );
    }

    /// The USD pin the table encoded per symbol: with a thin `EUR/USDT` also
    /// present, both routes are 2 legs, so the tie-break must keep USD.
    #[test]
    fn volume_blind_tie_break_keeps_the_usd_pivot() {
        let g = graph(&["EUR/USD", "USDC/USD", "EUR/USDT", "USDC/USDT"]);
        assert_eq!(
            route(&g, "EUR/USDC"),
            vec![("EUR/USD".into(), 1), ("USDC/USD".into(), -1)]
        );
    }

    /// Depth beats symbol order when weights are known: the thin pivot loses
    /// even though its symbols sort first.
    #[test]
    fn depth_ranks_by_the_thinnest_leg() {
        let g = graph(&["PYUSD/USDT", "PYUSD/USDC", "USDC/USD", "USDT/USD"]);
        let vol = |id: u64| -> f64 {
            let thin = crate::resolve_ticker_id("PYUSD/USDT");
            if id == thin { 1_000.0 } else { 90_000_000.0 }
        };
        let legs: Vec<String> = g
            .route_sym("PYUSD/USD", &vol)
            .expect("routes")
            .legs
            .iter()
            .map(|l| l.sym.to_string())
            .collect();
        assert_eq!(legs, vec!["PYUSD/USDC", "USDC/USD"], "thinnest leg decides");
    }

    /// An index quoted in its own currency must reach USD with no rule and no
    /// anchor knowledge. `classify_auto_cross_leg` rejected this outright.
    #[test]
    fn an_index_leg_composes_through_its_own_quote_currency() {
        let g = graph(&["GER40/EUR", "EUR/USD"]);
        assert_eq!(
            route(&g, "GER40/USD"),
            vec![("GER40/EUR".into(), 1), ("EUR/USD".into(), 1)]
        );
    }

    /// A single inverted primary is a one-leg route, not a missing pair.
    #[test]
    fn inversion_is_a_one_leg_route() {
        let g = graph(&["USD/JPY"]);
        assert_eq!(route(&g, "JPY/USD"), vec![("USD/JPY".into(), -1)]);
        assert_eq!(route(&g, "JPY-USD"), vec![("USD/JPY".into(), -1)]);
    }

    /// Provider identity is not in the graph at all, so the same two legs
    /// compose whichever forwarder saw them. Pinned as a contract.
    #[test]
    fn a_leg_is_a_leg_regardless_of_source() {
        let broker = graph(&["EUR/USD", "USD/JPY"]);
        let oracle = graph(&["USD/JPY", "EUR/USD"]);
        assert_eq!(route(&broker, "EUR/JPY"), route(&oracle, "EUR/JPY"));
    }

    /// `resolve_ticker_id("ETH/USD")` decodes base IP:2101 ("Ethereum Index",
    /// whose alias ETH shadows the crypto asset once the quote is fiat), while
    /// `ETH/USDT` decodes base CR:5801. The books must win, or ETH/USD strands
    /// on an asset that has no edges anywhere.
    #[test]
    fn a_shadowed_base_token_resolves_to_the_asset_with_the_books() {
        let g = graph(&["ETH/USDT", "USDT/USD"]);
        let (base, _) = g.assets_of("ETH/USD").expect("both tokens known");
        let (eth_cr, _) = ticker_assets(crate::resolve_ticker_id("ETH/USDT"));
        assert_eq!(base, eth_cr, "ETH must mean the asset that is quoted");
        assert_eq!(
            route(&g, "ETH/USD"),
            vec![("ETH/USDT".into(), 1), ("USDT/USD".into(), 1)]
        );
    }

    /// A 6-char primary registers no token by splitting, so the token map must
    /// fall back to the assets' own MITCH names/aliases.
    #[test]
    fn delimiterless_primaries_still_name_their_assets() {
        let g = graph(&["ETH/USDT", "USDT/USD", "EURUSD"]);
        let legs = route(&g, "ETH/EUR");
        assert_eq!(
            legs.last().map(|l| l.1),
            Some(-1),
            "EUR leg inverts: {legs:?}"
        );
        assert_eq!(legs.len(), 3, "ETH→USDT→USD→EUR: {legs:?}");
    }

    #[test]
    fn self_pairs_never_route() {
        let g = graph(&["EUR/USD", "USD/JPY"]);
        for sym in ["EUR/EUR", "USD/USD"] {
            assert!(g.route_sym(sym, &blind).is_none(), "{sym} must not route");
        }
    }

    #[test]
    fn a_missing_leg_is_a_clean_absence() {
        let g = graph(&["EUR/USD"]);
        assert!(g.route_sym("EUR/ZAR", &blind).is_none());
        assert!(g.route_sym("NOT/AREAL", &blind).is_none());
    }

    #[test]
    fn shortest_route_wins_over_a_longer_deeper_one() {
        // GBP/JPY direct, or GBP→USD→JPY. Depth favours the pivot route; hop
        // count must still take the direct book.
        let g = graph(&["GBP/JPY", "GBP/USD", "USD/JPY"]);
        let vol = |id: u64| -> f64 {
            if id == crate::resolve_ticker_id("GBP/JPY") {
                1.0
            } else {
                1e9
            }
        };
        let legs = g.route_sym("GBP/JPY", &vol).expect("routes").legs;
        assert_eq!(legs.len(), 1, "direct book wins on hops: {legs:?}");
    }

    /// A healthy leg: 2 ticking providers, fresh-weight ok, ~10 bps of CI.
    fn lq(bid: f64, ask: f64, conf: u16, age_ms: Option<i64>) -> LegQuote {
        LegQuote {
            tick: LegTick {
                bid,
                ask,
                mid: (bid + ask) * 0.5,
                conf,
            },
            age_ms,
            ci_ubp: crate::tdwap::encode_ci_ubp(100_000.0),
            conf_packed: Some(mitch::index::conf_pack_active(2, true)),
            accepted: 3,
        }
    }

    #[test]
    fn composed_age_is_the_oldest_leg_and_conf_the_weakest() {
        let g = graph(&["USD/CHF", "USD/JPY"]);
        let r = g.route_sym("CHF/JPY", &blind).expect("routes");
        let chf = crate::resolve_ticker_id("USD/CHF");
        let c = r
            .compose(|id| {
                Some(if id == chf {
                    lq(0.8, 0.8, 9_000, Some(4_200))
                } else {
                    lq(150.0, 150.0, 5_000, Some(120))
                })
            })
            .expect("composes");
        assert_eq!(c.age_ms, Some(4_200), "oldest leg, never the freshest");
        assert_eq!(c.conf, 5_000, "weakest leg");
        assert!((c.mid - 150.0 / 0.8).abs() < 1e-9);
    }

    #[test]
    fn an_unobserved_leg_makes_the_composed_age_unknown() {
        let g = graph(&["USD/CHF", "USD/JPY"]);
        let r = g.route_sym("CHF/JPY", &blind).expect("routes");
        let c = r
            .compose(|_| {
                Some(lq(1.0, 1.0, 10_000, None))
            })
            .expect("composes");
        assert!(c.age_ms.is_none(), "unknown age must not read as fresh");
    }

    #[test]
    fn compose_returns_none_when_a_leg_has_no_quote() {
        let g = graph(&["USD/CHF", "USD/JPY"]);
        let r = g.route_sym("CHF/JPY", &blind).expect("routes");
        let chf = crate::resolve_ticker_id("USD/CHF");
        assert!(
            r.compose(|id| (id == chf).then_some(lq(0.8, 0.8, 9_000, Some(1))))
            .is_none(),
            "one leg present must not fabricate a price"
        );
    }

    #[test]
    fn composed_bid_ask_stay_ordered_through_an_inversion() {
        let g = graph(&["USD/JPY"]);
        let r = g.route_sym("JPY/USD", &blind).expect("routes");
        let c = r
            .compose(|_| {
                Some(lq(150.0, 150.3, 9_000, Some(10)))
            })
            .expect("composes");
        assert!(c.bid <= c.ask);
        assert!((c.bid - 1.0 / 150.3).abs() < 1e-12);
        assert!((c.ask - 1.0 / 150.0).abs() < 1e-12);
    }

    /// A parity FX wrapper carries NO feed of its own, so it must route over its
    /// underlying currency's legs and mark IDENTICALLY: the peg is 1:1, so any
    /// difference at all would be invented.
    #[test]
    fn a_parity_wrap_marks_exactly_like_its_underlying() {
        let g = graph(&["EUR/USD", "USD/CAD", "USDC/USD"]);
        assert_eq!(route(&g, "EURC/USD"), vec![("EUR/USD".into(), 1)]);
        // CAD's primary is `USD/CAD`, so the wrap inherits the inversion.
        assert_eq!(route(&g, "QCAD/USD"), vec![("USD/CAD".into(), -1)]);
        let q = |_| Some(lq(1.0913, 1.0915, 9_000, Some(70)));
        for (wrap, under) in [("EURC/USD", "EUR/USD"), ("QCAD/USD", "CAD/USD")] {
            let w = g.route_sym(wrap, &blind).expect("routes").compose(q).expect("composes");
            let u = g.route_sym(under, &blind).expect("routes").compose(q).expect("composes");
            assert_eq!(w.mid, u.mid, "{wrap} mid must equal {under} EXACTLY");
            assert_eq!(w.bid, u.bid);
            assert_eq!(w.ask, u.ask);
            // Provenance is the underlying leg's, never wall clock.
            assert_eq!(w.age_ms, u.age_ms);
            assert_eq!(w.conf, u.conf);
        }
    }

    /// The peg is an asset identity, not a per-pair pin, so a wrap crosses
    /// anything its underlying can reach.
    #[test]
    fn a_parity_wrap_crosses_like_its_underlying() {
        let g = graph(&["EUR/USD", "USDC/USD", "USD/JPY"]);
        assert_eq!(
            route(&g, "EURC/USDC"),
            vec![("EUR/USD".into(), 1), ("USDC/USD".into(), -1)]
        );
        assert_eq!(
            route(&g, "EURC/JPY"),
            vec![("EUR/USD".into(), 1), ("USD/JPY".into(), 1)]
        );
    }

    /// A custodial BTC wrap has books of its own, so it must NOT be pegged away:
    /// `WBTC/USDT` stays the WBTC book, not BTC's.
    #[test]
    fn a_custodial_wrap_is_not_pegged_to_its_underlying() {
        let g = graph(&["WBTC/USDT", "BTC/USDT"]);
        assert_eq!(route(&g, "WBTC/USDT"), vec![("WBTC/USDT".into(), 1)]);
    }

    #[test]
    fn a_composed_output_offered_as_an_edge_is_the_callers_contract() {
        // The graph holds primaries only, so `USDT/JPY` (a triangulator output)
        // is absent and the route is built from the books instead.
        let g = graph(&["USDT/USD", "USD/JPY"]);
        assert_eq!(
            route(&g, "USDT/JPY"),
            vec![("USDT/USD".into(), 1), ("USD/JPY".into(), 1)]
        );
    }
}
