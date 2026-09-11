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

/// Total attempts across transport errors and 429s.
const MAX_ATTEMPTS: u32 = 3;
/// Backoff between attempts.
const RETRY_DELAY_MS: u64 = 500;

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
        .send()
        .map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status_code();
    let body = resp
        .body()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| format!("{url}: body read: {e}"))?;
    Ok((status, body))
}

/// Live premium lookup. `Ok((true, Some(uuid)))` = premium, `Ok((false, None))`
/// = cracked, `Err` = transport failure (caller must fail closed, no caching).
pub fn resolve_premium(name: &str) -> Result<(bool, Option<String>), String> {
    let mut last_err = String::new();
    'urls: for url in PROFILE_URLS {
        let mut attempts = 0;
        loop {
            attempts += 1;
            match fetch_once(&format!("{url}{name}")) {
                Ok((200, body)) => {
                    tracing::debug!("gln-auth: premium fetch ok via {url} for {name}");
                    if let Some(id) = parse_mojang_profile_response(200, &body) {
                        return Ok((true, Some(id)));
                    }
                    // 200 without a parsable id: fall through to the next endpoint.
                    continue 'urls;
                }
                Ok((204 | 404, _)) => return Ok((false, None)),
                Ok((429, _)) => last_err = format!("{url}: rate limited (429)"),
                Ok((status, _)) => last_err = format!("{url}: unexpected status {status}"),
                Err(e) => last_err = e,
            }
            if attempts >= MAX_ATTEMPTS {
                continue 'urls;
            }
            std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
        }
    }
    Err(format!("gln-auth: premium lookup failed for {name}: {last_err}"))
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
