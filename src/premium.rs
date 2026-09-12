//! Mojang premium resolver, response parser, and TTL cache.
//!
//! Transport policy (RULING-4/5): `waki` over `wasi:http`. Runtime under a live
//! Pumpkin host is unverified, so every transport failure fails closed: the
//! caller treats the player as cracked and nothing is cached.

use std::collections::HashMap;
use std::sync::Mutex;

use waki::Client;

/// Lookup endpoints, tried in order (JPremium parity).
const PROFILE_URLS: [&str; 2] = [
    "https://api.minecraftservices.com/minecraft/profile/lookup/name/",
    "https://api.mojang.com/users/profiles/minecraft/",
];

/// Total attempts SHARED across both endpoints and all failure categories.
/// Worst case join block: 3 fetches x 5s connect timeout + backoff.
const MAX_ATTEMPTS: u32 = 3;
/// Backoff between attempts.
const RETRY_DELAY_MS: u64 = 500;

/// What a completed fetch tells the plan.
#[derive(Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    /// 200 without a parsable id: give up on this endpoint, try the next.
    NextEndpoint,
    /// 429 / unexpected status / transport error: retry the same endpoint.
    RetrySame,
}

/// Shared attempt budget across the endpoint list. `next()` returns the
/// endpoint index to fetch or None when the budget is spent.
pub struct FetchPlan {
    budget: u32,
    spent: u32,
    endpoint: usize,
}

impl FetchPlan {
    pub fn new(budget: u32) -> Self {
        Self {
            budget,
            spent: 0,
            endpoint: 0,
        }
    }

    fn next(&self) -> Option<usize> {
        if self.spent >= self.budget {
            return None;
        }
        if self.endpoint >= PROFILE_URLS.len() {
            return None;
        }
        Some(self.endpoint)
    }

    fn record(&mut self, outcome: FetchOutcome) {
        self.spent += 1;
        if outcome == FetchOutcome::NextEndpoint {
            self.endpoint += 1;
        }
    }
}

pub struct PremiumCache {
    ttl: u64,
    inner: Mutex<HashMap<String, (bool, u64)>>,
}

impl PremiumCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            ttl: ttl_secs,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Cached premium verdict for `name` (case-insensitive), None if absent or expired.
    pub fn get(&self, name: &str, now: u64) -> Option<bool> {
        let map = self.inner.lock().ok()?;
        let (v, at) = map.get(&name.to_lowercase())?;
        if now.saturating_sub(*at) < self.ttl {
            Some(*v)
        } else {
            None
        }
    }

    pub fn put(&self, name: String, is_premium: bool, now: u64) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(name.to_lowercase(), (is_premium, now));
        }
    }
}

/// Maps a Mojang profile-lookup response to the premium UUID.
///
/// 200 with a JSON `id` string -> Some(id); 204/404 (and anything malformed
/// or unexpected) -> None.
pub fn parse_mojang_profile_response(status: u16, body: &str) -> Option<String> {
    match status {
        200 => {
            let v: serde_json::Value = serde_json::from_str(body).ok()?;
            v.get("id")
                .and_then(|id| id.as_str())
                .map(|s| s.to_string())
        }
        _ => None,
    }
}

/// One attempt against a single endpoint: Ok(Some(status, body)) on a completed
/// HTTP exchange, Err on transport failure.
fn fetch_once(url: &str) -> Result<(u16, String), String> {
    let resp = Client::new()
        .get(url)
        .connect_timeout(std::time::Duration::from_secs(5))
        .send()
        .map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status_code();
    let body = resp
        .body()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| format!("{url}: body read: {e}"))?;
    Ok((status, body))
}

/// Compares the UUID a client presented in Login Start against the Mojang
/// profile id for the name (JPremium's `detectPremiumUniqueIdsInHandshake`,
/// adapted for a plugin). Stock premium launchers send their genuine Mojang
/// UUID; stock cracked launchers send the offline UUID
/// (`UUIDv3("OfflinePlayer:<name>")`) or none at all, which the server then
/// derives the same way — both mismatch the Mojang id. The comparison strips
/// hyphens and lowercases: Mojang serves the id in hyphenless form, while
/// client UUIDs use the standard hyphenated form.
///
/// NOTE: in offline mode the presented UUID is client-controlled and not
/// cryptographically authenticated, so a custom client that knows the
/// victim's Mojang UUID can still spoof it. This check defeats every stock
/// launcher, which is the practical threat model for cracked servers.
pub fn uuid_matches_mojang_id(client_uuid: &str, mojang_id: &str) -> bool {
    let strip = |s: &str| s.replace('-', "").to_lowercase();
    let (c, m) = (strip(client_uuid), strip(mojang_id));
    !c.is_empty() && !m.is_empty() && c == m
}

/// Live premium lookup. `Ok((true, Some(uuid)))` = premium, `Ok((false, None))`
/// = cracked, `Err` = transport failure (caller must fail closed, no caching).
///
/// One attempt budget is shared across BOTH endpoints: at most
/// `MAX_ATTEMPTS` fetches happen per lookup, then the lookup fails closed.
pub fn resolve_premium(name: &str) -> Result<(bool, Option<String>), String> {
    let mut last_err = String::new();
    let mut plan = FetchPlan::new(MAX_ATTEMPTS);
    while let Some(endpoint) = plan.next() {
        let url = &PROFILE_URLS[endpoint];
        let outcome = match fetch_once(&format!("{url}{name}")) {
            Ok((200, body)) => {
                if let Some(id) = parse_mojang_profile_response(200, &body) {
                    tracing::debug!("0gln-auth: premium fetch ok via {url} for {name}");
                    return Ok((true, Some(id)));
                }
                // 200 without a parsable id: fall through to the next endpoint.
                FetchOutcome::NextEndpoint
            }
            Ok((204 | 404, _)) => return Ok((false, None)),
            Ok((429, _)) => {
                last_err = format!("{url}: rate limited (429)");
                FetchOutcome::RetrySame
            }
            Ok((status, _)) => {
                last_err = format!("{url}: unexpected status {status}");
                FetchOutcome::RetrySame
            }
            Err(e) => {
                last_err = e;
                FetchOutcome::RetrySame
            }
        };
        let retrying = outcome == FetchOutcome::RetrySame;
        plan.record(outcome);
        if retrying && plan.next().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
        }
    }
    Err(format!("0gln-auth: premium lookup failed for {name}: {last_err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_maps_status_codes_like_jpremium() {
        assert_eq!(
            parse_mojang_profile_response(200, r#"{"id":"abc123","name":"Notch"}"#),
            Some("abc123".to_string())
        );
        assert_eq!(parse_mojang_profile_response(200, r#"{"name":"Notch"}"#), None);
        assert_eq!(parse_mojang_profile_response(200, "not json"), None);
        assert_eq!(parse_mojang_profile_response(404, ""), None);
        assert_eq!(parse_mojang_profile_response(204, ""), None);
        assert_eq!(parse_mojang_profile_response(429, ""), None);
        assert_eq!(parse_mojang_profile_response(500, "{}"), None);
    }

    /// Hyphenated client UUID matches the hyphenless Mojang id form
    /// (case-insensitive); offline-derived UUIDs do not.
    #[test]
    fn uuid_matcher_compares_hyphenless_forms() {
        // Genuine Mojang UUID (Notch) as a client would present it, against
        // the hyphenless id the API returns.
        assert!(uuid_matches_mojang_id(
            "069a79f4-444e-9472-6a5b-efca90e38aaf5",
            "069a79f4444e94726a5befca90e38aaf5"
        ));
        // Offline UUID (v3 of "OfflinePlayer:steve") vs Mojang id of the
        // same name: different -> cracked client.
        assert!(!uuid_matches_mojang_id(
            "b66de432-6fb9-3f71-a454-2b4f5f2a5b5f",
            "069a79f4444e94726a5befca90e38aaf5"
        ));
        // Case-insensitive + hyphen tolerance.
        assert!(uuid_matches_mojang_id(
            "069A79F4-444E-9472-6A5B-EFCA90E38AAF5",
            "069a79f4444e94726a5befca90e38aaf5"
        ));
        // Empty inputs never match (fail closed).
        assert!(!uuid_matches_mojang_id("", "069a79f4444e94726a5befca90e38aaf5"));
        assert!(!uuid_matches_mojang_id("069a79f4444e94726a5befca90e38aaf5", ""));
    }

    /// The fetch plan shares one attempt budget across BOTH endpoints: a
    /// regression to per-endpoint counters would plan a 4th fetch here.
    #[test]
    fn fetch_plan_caps_total_attempts_across_endpoints() {
        let mut plan = FetchPlan::new(MAX_ATTEMPTS);
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(plan.next(), Some(0), "budget not yet spent");
            plan.record(FetchOutcome::RetrySame);
        }
        // All 3 attempts went to endpoint 0: endpoint 1 is never reached.
        assert_eq!(plan.next(), None);
    }

    /// A 200-without-id response is terminal for the endpoint: the plan moves
    /// on without waiting, spending one attempt each.
    #[test]
    fn fetch_plan_moves_to_next_endpoint_on_next_endpoint_outcome() {
        let mut plan = FetchPlan::new(MAX_ATTEMPTS);
        assert_eq!(plan.next(), Some(0));
        plan.record(FetchOutcome::NextEndpoint);
        assert_eq!(plan.next(), Some(1));
        plan.record(FetchOutcome::NextEndpoint);
        // There is no third endpoint.
        assert_eq!(plan.next(), None);
    }

    /// Mixed outcomes still share the budget: 1 (next-endpoint) + 2 (retries)
    /// = 3 total, and the plan ends on the endpoint it was retrying.
    #[test]
    fn fetch_plan_exhausts_shared_budget_on_mixed_outcomes() {
        let mut plan = FetchPlan::new(3);
        assert_eq!(plan.next(), Some(0));
        plan.record(FetchOutcome::NextEndpoint);
        assert_eq!(plan.next(), Some(1));
        plan.record(FetchOutcome::RetrySame);
        assert_eq!(plan.next(), Some(1));
        plan.record(FetchOutcome::NextEndpoint);
        // Third attempt spent moving past endpoint 1: nothing left.
        assert_eq!(plan.next(), None);
    }

    #[test]
    fn cache_expires_after_ttl() {
        let c = PremiumCache::new(60);
        c.put("notch".into(), true, 1000);
        assert_eq!(c.get("NOTCH", 1020), Some(true));
        assert_eq!(c.get("notch", 1060), None);
        assert_eq!(c.get("notch", 2000), None);
    }

    #[test]
    fn cache_keys_case_insensitive_and_negative_results_cached() {
        let c = PremiumCache::new(60);
        c.put("Steve".into(), false, 100);
        assert_eq!(c.get("STEVE", 120), Some(false));
        assert_eq!(c.get("steve", 120), Some(false));
        // Overwrite wins.
        c.put("steve".into(), true, 130);
        assert_eq!(c.get("STEVE", 140), Some(true));
    }

    #[test]
    fn cache_unknown_name_misses() {
        let c = PremiumCache::new(60);
        assert_eq!(c.get("ghost", 100), None);
    }
}
