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
        _ => "Unknown message.",
    }
}
