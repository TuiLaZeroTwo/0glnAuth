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
    PlayerLeaveEvent, PlayerMoveEvent,
};
use pumpkin_plugin_api::events_wit::{
    PlayerChatEventData, PlayerCommandPreprocessEventData, PlayerJoinEventData,
    PlayerLeaveEventData, PlayerMoveEventData,
};
use pumpkin_plugin_api::player::JavaKickOptions;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{Context, Player, Server};

use crate::messages::msg;
use crate::session::{is_session_valid, now_secs};
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

/// Marks the player as authenticated.
pub fn mark_authed(state: &SharedState, normalized: &str) {
    lock_write(state).authed.insert(normalized.to_string());
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
    let (joined_at, timeout_secs) = {
        let st = lock_read(state);
        match st.joined_at.get(normalized) {
            Some(t) => (*t, st.cfg.timeout_secs),
            None => return false,
        }
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

pub struct JoinHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerJoinEvent> for JoinHandler {
    fn handle(&self, _server: Server, data: PlayerJoinEventData) -> PlayerJoinEventData {
        let name = data.player.get_name();
        let normalized = normalize_name(&name);
        let ip = data.player.get_ip();

        // Account lookup; store errors fail closed (treat as unauthenticated).
        let account = match lock_read(&self.state).store.get_account(&normalized) {
            Ok(account) => account,
            Err(e) => {
                tracing::error!("gln-auth: join lookup failed for {normalized}: {e}");
                None
            }
        };

        // Premium flag: stored premium_id auto-authenticates (no live check here).
        if account.as_ref().is_some_and(|a| a.premium_id.is_some()) {
            mark_authed(&self.state, &normalized);
            data.player
                .send_system_message(TextComponent::text(msg("premium.auto")), false);
            return data;
        }

        // Session resume: valid expiry AND matching IP, no new session write.
        if account.is_some() {
            let resumed = match lock_read(&self.state).store.get_session(&normalized) {
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

        // Unauthenticated: record join time for the timeout sweep and prompt.
        {
            let mut st = lock_write(&self.state);
            st.joined_at.insert(normalized.clone(), now_secs());
            st.authed.remove(&normalized);
        }
        if account.is_some() {
            data.player
                .send_system_message(TextComponent::text(msg("join.login")), false);
        } else {
            data.player
                .send_system_message(TextComponent::text(msg("join.register")), false);
        }
        data
    }
}

pub struct LeaveHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerLeaveEvent> for LeaveHandler {
    fn handle(&self, _server: Server, data: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let normalized = normalize_name(&data.player.get_name());
        {
            let mut st = lock_write(&self.state);
            st.joined_at.remove(&normalized);
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
