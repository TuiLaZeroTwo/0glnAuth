use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub timeout_secs: u64,
    pub session_minutes: u64,
    pub max_login_tries: u32,
    pub pw_min_len: usize,
    pub pw_max_len: usize,
    pub name_regex: String,
    pub premium_check_enabled: bool,
    pub premium_cache_minutes: u64,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 120,
            session_minutes: 120,
            max_login_tries: 5,
            pw_min_len: 8,
            pw_max_len: 64,
            name_regex: r"^[a-zA-Z0-9_]{3,16}$".to_string(),
            premium_check_enabled: true,
            premium_cache_minutes: 45,
        }
    }
}

pub fn load_config(path: &str) -> PluginConfig {
    match std::fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content).unwrap_or_default(),
        Err(_) => PluginConfig::default(),
    }
}

pub fn default_config_toml() -> String {
    toml::to_string_pretty(&PluginConfig::default()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_global_constraints() {
        let cfg = PluginConfig::default();
        assert_eq!(cfg.timeout_secs, 120);
        assert_eq!(cfg.session_minutes, 120);
        assert_eq!(cfg.max_login_tries, 5);
        assert_eq!(cfg.pw_min_len, 8);
        assert_eq!(cfg.pw_max_len, 64);
        assert!(cfg.premium_check_enabled);
        assert_eq!(cfg.premium_cache_minutes, 45);
    }
}
