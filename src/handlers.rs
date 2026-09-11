//! Join/leave handlers, unauthenticated freeze, and login-timeout kick.
//!
//! Fail-closed rules enforced here:
//! - store errors on join are treated as "unauthenticated": the player is
//!   prompted to login and frozen until they do,
//! - session resume requires a valid expiry AND a matching IP,
//! - players that exceed `timeout_secs` without authenticating are kicked,
//! - frozen players can only run auth commands from the allowlist.

use std::sync::{RwLockReadGuard, RwLockWriteGuard};

use pumpkin_plugin_api::events::{
    EventHandler, EventPriority, PlayerChatEvent, PlayerCommandPreprocessEvent, PlayerJoinEvent,
    PlayerLeaveEvent, PlayerMoveEvent, PlayerPreLoginEvent,
};
use pumpkin_plugin_api::events_wit::{
    PlayerChatEventData, PlayerCommandPreprocessEventData, PlayerJoinEventData,
    PlayerLeaveEventData, PlayerMoveEventData, PlayerPreLoginEventData,
};
use pumpkin_plugin_api::player::JavaKickOptions;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{Context, Player, Server};

use crate::messages::msg;
use crate::premium::resolve_premium;
use crate::session::{is_session_valid, now_secs};
use crate::storage::Account;
use crate::validation::normalize_name;
use crate::{AppState, SharedState};

/// Commands an unauthenticated player may still run (matched case-insensitively
/// against the first word of the command string, with or without the leading `/`).
const ALLOWED_COMMANDS: [&str; 8] = [
    "login",
    "register",
    "logout",
    "l",
    "log",
    "changepassword",
    "unregister",
    "setpremium",
];

fn lock_read(state: &SharedState) -> RwLockReadGuard<'_, AppState> {
    state.read().unwrap_or_else(|e| e.into_inner())
}

fn lock_write(state: &SharedState) -> RwLockWriteGuard<'_, AppState> {
    state.write().unwrap_or_else(|e| e.into_inner())
}

/// True if the player with this (already normalized) name is authenticated.
pub fn is_authed(state: &SharedState, normalized: &str) -> bool {
    lock_read(state).authed.contains(normalized)
}

/// Which join prompt the player should get.
pub enum AccountStatus {
    /// No stored account: prompt /register.
    Missing,
    /// Stored account: prompt /login.
    Exists,
    /// Store error: fail closed as unauthenticated, but prompt /login — an
    /// existing account would be pointed at the wrong command by /register.
    Unknown,
}

/// Message key for the join prompt, per account status (fail-closed: both
/// unknown and missing leave the player unauthenticated).
pub fn join_prompt_key(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Missing => "join.register",
        AccountStatus::Exists | AccountStatus::Unknown => "join.login",
    }
}

/// Records a failed login for `ip` and reports whether the per-IP limit is
/// now reached (>= max tries). Caps the stored counter so it cannot wrap.
pub fn record_failure(state: &SharedState, ip: &str, max_tries: u32) -> bool {
    let mut st = lock_write(state);
    let entry = st.failures.entry(ip.to_string()).or_insert(0);
    *entry = entry.saturating_add(1).min(max_tries);
    is_rate_limited(*entry, max_tries)
}

/// True when `attempts` reached the configured kick threshold.
fn is_rate_limited(attempts: u32, max_tries: u32) -> bool {
    attempts >= max_tries
}

/// Kicks the player with the rate-limit message and cleans up join bookkeeping.
/// Returns true if the player was kicked.
pub fn kick_rate_limited(player: &Player, state: &SharedState, normalized: &str) -> bool {
    if let Some(java) = player.as_java() {
        java.kick(JavaKickOptions::new(TextComponent::text(msg(
            "login.rate_limited",
        ))));
    } else if let Some(bedrock) = player.as_bedrock() {
        bedrock.kick(&pumpkin_plugin_api::player::BedrockKickOptions::new(
            pumpkin_plugin_api::player::BedrockDisconnectReason::Kicked,
            msg("login.rate_limited"),
        ));
    } else {
        return false;
    }
    let mut st = lock_write(state);
    st.joined_at.remove(normalized);
    st.authed.remove(normalized);
    true
}

/// Marks the player as authenticated and clears their login-timeout bookkeeping.
pub fn mark_authed(state: &SharedState, normalized: &str) {
    let mut st = lock_write(state);
    st.authed.insert(normalized.to_string());
    st.joined_at.remove(normalized);
}

/// Marks the player as unauthenticated.
pub fn mark_unauthed(state: &SharedState, normalized: &str) {
    lock_write(state).authed.remove(normalized);
}

/// True if the player joined more than `timeout_secs` ago and is still unauthenticated.
fn is_timed_out(joined_at: u64, now: u64, timeout_secs: u64) -> bool {
    now.saturating_sub(joined_at) > timeout_secs
}

/// Kicks the player with the timeout message and cleans up join bookkeeping.
/// Returns true if the player was kicked.
fn kick_timeout(player: &Player, state: &SharedState, normalized: &str) -> bool {
    if let Some(java) = player.as_java() {
        java.kick(JavaKickOptions::new(TextComponent::text(msg("timeout.kick"))));
    } else if let Some(bedrock) = player.as_bedrock() {
        bedrock.kick(&pumpkin_plugin_api::player::BedrockKickOptions::new(
            pumpkin_plugin_api::player::BedrockDisconnectReason::Kicked,
            msg("timeout.kick"),
        ));
    } else {
        return false;
    }
    let mut st = lock_write(state);
    st.joined_at.remove(normalized);
    st.authed.remove(normalized);
    true
}

/// Runs the timeout sweep for a player: kicks them if they exceeded the login
/// timeout. Returns true when the player was kicked.
fn timeout_sweep(player: &Player, state: &SharedState, normalized: &str) -> bool {
    let lookup = {
        let st = lock_read(state);
        st.joined_at
            .get(normalized)
            .map(|t| (*t, st.cfg.timeout_secs))
    };
    let Some((joined_at, timeout_secs)) = lookup else {
        return false;
    };
    if is_timed_out(joined_at, now_secs(), timeout_secs) {
        kick_timeout(player, state, normalized)
    } else {
        false
    }
}

/// True if the command string is allowed for frozen (unauthenticated) players.
/// Accepts the raw preprocess string with or without a leading `/`.
fn is_command_allowed(command: &str) -> bool {
    let trimmed = command.trim_start_matches('/');
    let first = trimmed
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("");
    let lowered = first.to_lowercase();
    ALLOWED_COMMANDS.iter().any(|allowed| lowered == *allowed)
}

/// Registers all event handlers with the plugin context.
pub fn register_handlers(context: &Context, state: SharedState) -> Result<(), String> {
    context
        .register_event_handler(
            PreLoginHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("gln-auth: failed to register pre-login handler: {e}"))?;
    context
        .register_event_handler(
            JoinHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("gln-auth: failed to register join handler: {e}"))?;
    context
        .register_event_handler(
            LeaveHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("gln-auth: failed to register leave handler: {e}"))?;
    context
        .register_event_handler(
            FreezeMoveHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("gln-auth: failed to register move handler: {e}"))?;
    context
        .register_event_handler(
            FreezeChatHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("gln-auth: failed to register chat handler: {e}"))?;
    context
        .register_event_handler(
            FreezeCommandHandler { state },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("gln-auth: failed to register command handler: {e}"))?;
    Ok(())
}

pub struct PreLoginHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerPreLoginEvent> for PreLoginHandler {
    /// Single-session rule: deny a second simultaneous join with the same
    /// name while the first is authenticated. Runs before a Player handle
    /// exists, so the new connection is cancelled here with a kick message.
    fn handle(&self, _server: Server, mut data: PlayerPreLoginEventData) -> PlayerPreLoginEventData {
        let normalized = normalize_name(&data.player_name);
        if is_authed(&self.state, &normalized) {
            tracing::warn!(
                "gln-auth: denied duplicate join for {normalized} from {}",
                data.ip_address
            );
            data.cancelled = true;
            data.kick_message = TextComponent::text(msg("login.duplicate"));
        }
        data
    }
}

pub struct JoinHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerJoinEvent> for JoinHandler {
    fn handle(&self, _server: Server, data: PlayerJoinEventData) -> PlayerJoinEventData {
        let name = data.player.get_name();
        let normalized = normalize_name(&name);
        let ip = data.player.get_ip();

        // Account lookup; store errors fail closed (treat as unauthenticated).
        // The account-existence tri-state drives both auto-login checks and
        // the final prompt: Unknown prompts /login, never /register.
        let (account, status) = match lock_read(&self.state).store.get_account(&normalized) {
            Ok(Some(a)) => (Some(a), AccountStatus::Exists),
            Ok(None) => (None, AccountStatus::Missing),
            Err(e) => {
                tracing::error!("gln-auth: join lookup failed for {normalized}: {e}");
                (None, AccountStatus::Unknown)
            }
        };

        // 1. Stored premium_id auto-authenticates (no live check here).
        if account.as_ref().is_some_and(|a| a.premium_id.is_some()) {
            mark_authed(&self.state, &normalized);
            data.player
                .send_system_message(TextComponent::text(msg("premium.auto")), false);
            return data;
        }

        // 2. Live premium check: no account, or an account without premium_id.
        let check_enabled = lock_read(&self.state).cfg.premium_check_enabled;
        if check_enabled && account.as_ref().is_none_or(|a| a.premium_id.is_none()) {
            match self.premium_verdict(&normalized, now_secs()) {
                PremiumVerdict::Premium(id) => {
                    if self.persist_premium(&data, &normalized, &name, &ip, id) {
                        mark_authed(&self.state, &normalized);
                        data.player.send_system_message(
                            TextComponent::text(msg("premium.auto")),
                            false,
                        );
                        return data;
                    }
                    // Persist failure: fall through to the prompt, fail closed.
                }
                PremiumVerdict::Cracked => {}
                PremiumVerdict::Unavailable => {}
            }
        }

        // 3. Session resume: valid expiry AND matching IP, no new session write.
        if account.is_some() {
            let sess = lock_read(&self.state).store.get_session(&normalized);
            let resumed = match sess {
                Ok(Some(session)) => is_session_valid(&session, now_secs(), &ip),
                Ok(None) => false,
                Err(e) => {
                    tracing::error!("gln-auth: join session lookup failed for {normalized}: {e}");
                    false
                }
            };
            if resumed {
                mark_authed(&self.state, &normalized);
                data.player
                    .send_system_message(TextComponent::text(msg("session.resume")), false);
                return data;
            }
        }

        // 4. Unauthenticated: record join time for the timeout sweep and prompt.
        {
            let mut st = lock_write(&self.state);
            st.joined_at.insert(normalized.clone(), now_secs());
            st.authed.remove(&normalized);
        }
        data.player
            .send_system_message(TextComponent::text(msg(join_prompt_key(status))), false);
        data
    }
}

/// Outcome of the join-time premium check.
enum PremiumVerdict {
    Premium(String),
    Cracked,
    Unavailable,
}

impl JoinHandler {
    /// Cache-then-live premium lookup. Only live resolutions are cached;
    /// transport failures (fail closed) leave the cache untouched.
    fn premium_verdict(&self, normalized: &str, now: u64) -> PremiumVerdict {
        let cache_hit = { lock_read(&self.state).premium.get(normalized, now) };
        if cache_hit.is_some() {
            // A cached `false` is a known cracked verdict; a cached `true`
            // without a stored premium_id (earlier persist failure, deleted
            // account) cannot auto-login without the id: fail closed, prompt.
            return PremiumVerdict::Cracked;
        }
        match resolve_premium(normalized) {
            Ok((true, Some(id))) => {
                lock_read(&self.state)
                    .premium
                    .put(normalized.to_string(), true, now);
                PremiumVerdict::Premium(id)
            }
            Ok((false, _)) => {
                lock_read(&self.state)
                    .premium
                    .put(normalized.to_string(), false, now);
                PremiumVerdict::Cracked
            }
            Ok((true, None)) => PremiumVerdict::Cracked,
            Err(e) => {
                tracing::warn!("gln-auth: premium lookup failed for {normalized}: {e}");
                PremiumVerdict::Unavailable
            }
        }
    }

    /// Persists the premium_id: updates an existing account or creates a new
    /// one for a first-join premium player. True on success.
    fn persist_premium(
        &self,
        data: &PlayerJoinEventData,
        normalized: &str,
        name: &str,
        ip: &str,
        id: String,
    ) -> bool {
        let uuid = data.player.get_id().to_string();
        let now = now_secs();
        let account = match lock_read(&self.state).store.get_account(normalized) {
            Ok(Some(mut existing)) => {
                existing.premium_id = Some(id);
                existing.last_ip = ip.to_string();
                existing.last_seen = now as i64;
                existing
            }
            Ok(None) => Account {
                name: name.to_string(),
                uuid,
                premium_id: Some(id),
                hash: String::new(),
                last_ip: ip.to_string(),
                last_seen: now as i64,
                created: now as i64,
            },
            Err(e) => {
                tracing::error!("gln-auth: premium persist lookup failed for {normalized}: {e}");
                return false;
            }
        };
        if let Err(e) = lock_read(&self.state).store.save_account(&account) {
            tracing::error!("gln-auth: premium persist save failed for {normalized}: {e}");
            return false;
        }
        true
    }
}

pub struct LeaveHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerLeaveEvent> for LeaveHandler {
    fn handle(&self, _server: Server, data: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let normalized = normalize_name(&data.player.get_name());
        let ip = data.player.get_ip();
        {
            let mut st = lock_write(&self.state);
            st.joined_at.remove(&normalized);
            st.failures.remove(&ip);
        }
        mark_unauthed(&self.state, &normalized);
        // The session row is kept on purpose so a relog within the window resumes.
        data
    }
}

/// Shared freeze logic: kicks timed-out players, returns true when the event
/// must be cancelled (player unauthenticated).
fn freeze_check(player: &Player, state: &SharedState, normalized: &str) -> bool {
    if is_authed(state, normalized) {
        return false;
    }
    timeout_sweep(player, state, normalized);
    // Cancel whether or not the kick succeeded: the player is unauthenticated.
    true
}

pub struct FreezeMoveHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerMoveEvent> for FreezeMoveHandler {
    fn handle(&self, _server: Server, mut data: PlayerMoveEventData) -> PlayerMoveEventData {
        let normalized = normalize_name(&data.player.get_name());
        if freeze_check(&data.player, &self.state, &normalized) {
            data.cancelled = true;
        }
        data
    }
}

pub struct FreezeChatHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerChatEvent> for FreezeChatHandler {
    fn handle(&self, _server: Server, mut data: PlayerChatEventData) -> PlayerChatEventData {
        let normalized = normalize_name(&data.player.get_name());
        if freeze_check(&data.player, &self.state, &normalized) {
            data.cancelled = true;
        }
        data
    }
}

pub struct FreezeCommandHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerCommandPreprocessEvent> for FreezeCommandHandler {
    fn handle(
        &self,
        _server: Server,
        mut data: PlayerCommandPreprocessEventData,
    ) -> PlayerCommandPreprocessEventData {
        let normalized = normalize_name(&data.player.get_name());
        if is_authed(&self.state, &normalized) {
            return data;
        }
        if !is_command_allowed(&data.command) {
            data.cancelled = true;
            return data;
        }
        if timeout_sweep(&data.player, &self.state, &normalized) {
            data.cancelled = true;
        }
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authed_set_tracks_login() {
        let mut set = std::collections::HashSet::new();
        set.insert("steve".to_string());
        assert!(set.contains("steve"));
        set.remove("steve");
        assert!(!set.contains("steve"));
    }

    /// Unknown account existence (store error) must prompt /login, not
    /// /register: prompting register on a store error points an existing
    /// account at the wrong command. Missing -> register, Exists -> login.
    #[test]
    fn join_prompt_falls_back_to_login_when_account_unknown() {
        assert_eq!(join_prompt_key(AccountStatus::Missing), "join.register");
        assert_eq!(join_prompt_key(AccountStatus::Exists), "join.login");
        assert_eq!(join_prompt_key(AccountStatus::Unknown), "join.login");
    }

    /// The kick boundary: exactly max_login_tries failures kicks; one below
    /// does not; already-above still kicks (lowering the cap mid-flight).
    #[test]
    fn rate_limit_kicks_at_or_beyond_max_tries() {
        assert!(!is_rate_limited(4, 5));
        assert!(is_rate_limited(5, 5));
        assert!(is_rate_limited(6, 5));
    }

    #[test]
    fn timeout_boundary_matches_config() {
        // joined_at=1000, timeout 120s: 1100 is fine, 1120 is the boundary, 1121 kicks.
        assert!(!is_timed_out(1000, 1100, 120));
        assert!(!is_timed_out(1000, 1120, 120));
        assert!(is_timed_out(1000, 1121, 120));
        // A clock running backwards must not kick (saturating).
        assert!(!is_timed_out(2000, 1000, 120));
    }

    #[test]
    fn allowlist_matches_first_word_case_insensitive() {
        assert!(is_command_allowed("login secret"));
        assert!(is_command_allowed("/login secret"));
        assert!(is_command_allowed("/LOGIN secret"));
        assert!(is_command_allowed("l secret"));
        assert!(is_command_allowed("/register a b"));
        assert!(is_command_allowed("/changepassword old new"));
        assert!(is_command_allowed("/unregister pw"));
        assert!(is_command_allowed("/setpremium steve on"));
        assert!(is_command_allowed("register"));
        assert!(!is_command_allowed("say hello"));
        assert!(!is_command_allowed("/give steve diamond"));
        assert!(!is_command_allowed("/"));
        assert!(!is_command_allowed(""));
        // Prefixes must not leak through: "loginx" is not "login".
        assert!(!is_command_allowed("loginx"));
    }
}
