//! Default message table.
//!
//! All player-facing strings live here as defaults; owners can override any
//! of them via the `[messages]` section of `config.toml` (see
//! `PluginConfig::message`). The fallback for a key missing from the config
//! map is the default value from this table, so old configs keep working.

use std::collections::HashMap;

/// Default text for every player-facing message, keyed by message key.
pub fn default_messages() -> HashMap<String, String> {
    let mut m = HashMap::new();
    let entries = [
        ("join.register", "Please register: /register <password> <confirm>"),
        (
            "join.choice",
            "Are you premium or cracked? Use /premium to verify a premium name, or /register <password> <confirm> to create a cracked account.",
        ),
        ("join.login", "Please login: /login <password>"),
        ("login.ok", "Logged in."),
        ("login.wrong", "Wrong password."),
        ("login.already", "You are already logged in."),
        ("login.duplicate", "Already logged in from another session."),
        ("login.rate_limited", "Too many failed login attempts."),
        ("register.ok", "Registered and logged in."),
        ("register.mismatch", "Passwords do not match."),
        ("register.exists", "An account with this name already exists. Use /login."),
        ("session.resume", "Session resumed, welcome back."),
        ("premium.auto", "Premium account detected, logged in."),
        ("premium.ok", "Premium verified, welcome."),
        ("premium.not", "That name is not premium, use /register."),
        (
            "premium.unavailable",
            "Could not verify premium status, try again later.",
        ),
        (
            "premium.nopass",
            "Account has no password, set a password first.",
        ),
        ("premium.exists", "An account with this name already exists, use /login."),
        (
            "premium.uuid_mismatch",
            "This is a premium name: join with your premium (Mojang-signed-in) launcher.",
        ),
        (
            "premium.uuid_wrong",
            "You are not using this account's launcher: premium verification requires joining with the Mojang-signed-in launcher.",
        ),
        ("timeout.kick", "Login timeout."),
    ];
    for (k, v) in entries {
        m.insert(k.to_string(), v.to_string());
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key used by handlers/commands must exist in the defaults map:
    /// a missing key would silently render as an empty/fallback string.
    #[test]
    fn default_messages_cover_all_plugin_keys() {
        let m = default_messages();
        for key in [
            "join.register",
            "join.choice",
            "join.login",
            "login.ok",
            "login.wrong",
            "login.already",
            "login.duplicate",
            "login.rate_limited",
            "register.ok",
            "register.mismatch",
            "register.exists",
            "session.resume",
            "premium.auto",
            "premium.ok",
            "premium.not",
            "premium.unavailable",
            "premium.nopass",
            "premium.exists",
            "premium.uuid_mismatch",
            "premium.uuid_wrong",
            "timeout.kick",
        ] {
            assert!(m.contains_key(key), "missing default for key {key}");
        }
        assert_eq!(
            m["login.duplicate"],
            "Already logged in from another session."
        );
        assert_eq!(
            m["premium.not"],
            "That name is not premium, use /register."
        );
    }
}
