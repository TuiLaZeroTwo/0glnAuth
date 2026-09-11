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

/// Loads and parses the plugin config. Both a missing file and a malformed
/// file are errors: the caller must abort the load instead of silently
/// running with defaults (a mistyped key would otherwise go unnoticed).
pub fn load_config(path: &str) -> Result<PluginConfig, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("gln-auth: cannot read config {path}: {e}"))?;
    toml::from_str(&content).map_err(|e| format!("gln-auth: invalid config {path}: {e}"))
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

    #[test]
    fn load_config_missing_file_is_an_error() {
        let missing = std::env::temp_dir().join("gln-auth-config-does-not-exist-9.toml");
        let _ = std::fs::remove_file(&missing);
        assert!(load_config(missing.to_str().unwrap()).is_err());
    }

    #[test]
    fn load_config_invalid_toml_is_an_error() {
        let path = std::env::temp_dir().join("gln-auth-config-invalid-9.toml");
        std::fs::write(&path, "not [valid toml !!").unwrap();
        assert!(load_config(path.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_config_parses_full_override() {
        let path = std::env::temp_dir().join("gln-auth-config-full-9.toml");
        std::fs::write(
            &path,
            "timeout_secs = 60\nsession_minutes = 30\nmax_login_tries = 2\npw_min_len = 6\npw_max_len = 32\nname_regex = 'a+'\npremium_check_enabled = false\npremium_cache_minutes = 5\n",
        )
        .unwrap();
        let cfg = load_config(path.to_str().unwrap()).expect("parse");
        assert_eq!(cfg.timeout_secs, 60);
        assert_eq!(cfg.session_minutes, 30);
        assert_eq!(cfg.max_login_tries, 2);
        assert_eq!(cfg.pw_min_len, 6);
        assert_eq!(cfg.pw_max_len, 32);
        assert_eq!(cfg.name_regex, "a+");
        assert!(!cfg.premium_check_enabled);
        assert_eq!(cfg.premium_cache_minutes, 5);
        let _ = std::fs::remove_file(&path);
    }

    /// The example config shipped in the repo must parse and contain every
    /// key with its default value: a drift between the example and the real
    /// defaults (or a typo'd key the parser would silently ignore via
    /// serde's unknown-field tolerance) would ship a broken example.
    /// Comment lines must not break the parse.
    #[test]
    fn example_config_parses_and_matches_defaults() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins")
            .join("gln-auth")
            .join("config.toml");
        let example = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let cfg = toml::from_str::<PluginConfig>(&example)
            .unwrap_or_else(|e| panic!("example config must parse: {e}"));
        let defaults = PluginConfig::default();
        assert_eq!(cfg.timeout_secs, defaults.timeout_secs);
        assert_eq!(cfg.session_minutes, defaults.session_minutes);
        assert_eq!(cfg.max_login_tries, defaults.max_login_tries);
        assert_eq!(cfg.pw_min_len, defaults.pw_min_len);
        assert_eq!(cfg.pw_max_len, defaults.pw_max_len);
        assert_eq!(cfg.name_regex, defaults.name_regex);
        assert_eq!(cfg.premium_check_enabled, defaults.premium_check_enabled);
        assert_eq!(cfg.premium_cache_minutes, defaults.premium_cache_minutes);
    }
}
