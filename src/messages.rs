pub fn msg(key: &str) -> &'static str {
    match key {
        "join.register" => "Please register: /register <password> <confirm>",
        "join.login" => "Please login: /login <password>",
        "login.ok" => "Logged in.",
        "login.wrong" => "Wrong password.",
        "register.ok" => "Registered and logged in.",
        "register.mismatch" => "Passwords do not match.",
        "session.resume" => "Session resumed, welcome back.",
        "premium.auto" => "Premium account detected, logged in.",
        "timeout.kick" => "Login timeout.",
        "login.duplicate" => "Already logged in from another session.",
        "login.rate_limited" => "Too many failed login attempts.",
        _ => "Unknown message.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hardening keys must resolve to their real strings; a renamed or
    /// dropped key would silently fall back to "Unknown message." in the UI.
    #[test]
    fn hardening_keys_resolve_to_real_strings() {
        assert_eq!(
            msg("login.duplicate"),
            "Already logged in from another session."
        );
        assert_eq!(msg("login.rate_limited"), "Too many failed login attempts.");
    }
}
