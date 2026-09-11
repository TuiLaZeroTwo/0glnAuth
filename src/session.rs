use crate::config::PluginConfig;
use crate::storage::Session;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_expiry(now: u64, cfg: &PluginConfig) -> u64 {
    now + cfg.session_minutes * 60
}

pub fn is_session_valid(sess: &Session, now: u64, ip: &str) -> bool {
    now < sess.expires_at && sess.ip == ip
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_requires_fresh_expiry_and_same_ip() {
        let s = Session { name: "steve".into(), ip: "1.2.3.4".into(), expires_at: 200 };
        assert!(is_session_valid(&s, 100, "1.2.3.4"));
        assert!(!is_session_valid(&s, 300, "1.2.3.4"));
        assert!(!is_session_valid(&s, 100, "9.9.9.9"));
        assert_eq!(new_expiry(1000, &PluginConfig::default()), 1000 + 120 * 60);
    }
}
