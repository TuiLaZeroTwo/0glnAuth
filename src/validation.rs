use crate::config::PluginConfig;

pub fn normalize_name(name: &str) -> String {
    name.to_lowercase()
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.len() < 3 || name.len() > 16 {
        return Err("Name must be 3-16 chars.".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("Name may only contain a-z A-Z 0-9 _.".to_string());
    }
    Ok(())
}

pub fn validate_password(pw: &str, name: &str, cfg: &PluginConfig) -> Result<(), String> {
    if pw.len() < cfg.pw_min_len || pw.len() > cfg.pw_max_len {
        return Err(format!(
            "Password must be {}-{} chars.",
            cfg.pw_min_len, cfg.pw_max_len
        ));
    }
    if pw.to_lowercase().contains(&name.to_lowercase()) {
        return Err("Password must not contain your name.".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::PluginConfig;
    use crate::validation::{normalize_name, validate_password};

    #[test]
    fn rejects_short_password_and_name_in_password() {
        let cfg = PluginConfig::default();
        assert!(validate_password("short", "steve", &cfg).is_err());
        assert!(validate_password("steve12345", "steve", &cfg).is_err());
        assert!(validate_password("s3cure!Pass9", "steve", &cfg).is_ok());
        assert_eq!(normalize_name("Steve"), "steve");
    }
}
