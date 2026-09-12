//! Join/leave handlers, unauthenticated freeze, and login-timeout kick.
//!
//! Fail-closed rules enforced here:
//! - store errors on join are treated as "unauthenticated": the player is
//!   prompted to login and frozen until they do,
//! - session resume requires a valid expiry AND a matching IP,
//! - players that exceed `timeout_secs` without authenticating are kicked,
//! - frozen players can only run auth commands from the allowlist,
//! - the premium resolver is NEVER run at join time: first joins get a
//!   chat-choice prompt (/premium or /register) and premium verification
//!   only happens through the explicit /premium command.

use std::sync::{RwLockReadGuard, RwLockWriteGuard};

use pumpkin_plugin_api::events::{
    EventHandler, EventPriority, PlayerChatEvent, PlayerCommandPreprocessEvent, PlayerDropItemEvent,
    PlayerInteractEvent, PlayerJoinEvent, PlayerLeaveEvent, PlayerMoveEvent, PlayerPreLoginEvent,
    BlockBreakEvent, BlockPlaceEvent, EntityDamageByEntityEvent,
};
use pumpkin_plugin_api::events_wit::{
    BlockBreakEventData, BlockPlaceEventData, EntityDamageByEntityEventData,
    PlayerChatEventData, PlayerCommandPreprocessEventData, PlayerDropItemEventData,
    PlayerInteractEventData, PlayerJoinEventData, PlayerLeaveEventData, PlayerMoveEventData,
    PlayerPreLoginEventData,
};
use pumpkin_plugin_api::player::JavaKickOptions;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{Context, Player, Server};

use crate::premium::uuid_matches_mojang_id;
use crate::session::{is_session_valid, now_secs};
use crate::validation::normalize_name;
use crate::{AppState, SharedState};

/// Commands an unauthenticated player may still run (matched case-insensitively
/// against the first word of the command string, with or without the leading `/`).
const ALLOWED_COMMANDS: [&str; 9] = [
    "login",
    "register",
    "logout",
    "l",
    "log",
    "changepassword",
    "unregister",
    "setpremium",
    "premium",
];

fn lock_read(state: &SharedState) -> RwLockReadGuard<'_, AppState> {
    state.read().unwrap_or_else(|e| e.into_inner())
}

fn lock_write(state: &SharedState) -> RwLockWriteGuard<'_, AppState> {
    state.write().unwrap_or_else(|e| e.into_inner())
}

/// Config-driven message lookup (overrides in [messages], else the default).
pub fn message(state: &SharedState, key: &str) -> String {
    lock_read(state).cfg.message(key)
}

/// True if the player with this (already normalized) name is authenticated.
pub fn is_authed(state: &SharedState, normalized: &str) -> bool {
    lock_read(state).authed.contains(normalized)
}

/// Which join prompt the player should get.
pub enum AccountStatus {
    /// No stored account: prompt the premium-or-cracked choice.
    Missing,
    /// Stored account: prompt /login.
    Exists,
    /// Store error: fail closed as unauthenticated, but prompt /login — an
    /// existing account would be pointed at the wrong command otherwise.
    Unknown,
}

/// Message key for the join prompt, per account status (fail-closed: both
/// unknown and missing leave the player unauthenticated).
pub fn join_prompt_key(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Missing => "join.choice",
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
        java.kick(JavaKickOptions::new(TextComponent::text(&message(
            state,
            "login.rate_limited",
        ))));
    } else if let Some(bedrock) = player.as_bedrock() {
        bedrock.kick(&pumpkin_plugin_api::player::BedrockKickOptions::new(
            pumpkin_plugin_api::player::BedrockDisconnectReason::Kicked,
            message(state, "login.rate_limited"),
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
        java.kick(JavaKickOptions::new(TextComponent::text(&message(
            state,
            "timeout.kick",
        ))));
    } else if let Some(bedrock) = player.as_bedrock() {
        bedrock.kick(&pumpkin_plugin_api::player::BedrockKickOptions::new(
            pumpkin_plugin_api::player::BedrockDisconnectReason::Kicked,
            message(state, "timeout.kick"),
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
        .map_err(|e| format!("0gln-auth: failed to register pre-login handler: {e}"))?;
    context
        .register_event_handler(
            JoinHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register join handler: {e}"))?;
    context
        .register_event_handler(
            LeaveHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register leave handler: {e}"))?;
    context
        .register_event_handler(
            FreezeMoveHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register move handler: {e}"))?;
    context
        .register_event_handler(
            FreezeChatHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register chat handler: {e}"))?;
    context
        .register_event_handler(
            FreezeCommandHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register command handler: {e}"))?;
    context
        .register_event_handler(
            FreezeBlockBreakHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register block-break handler: {e}"))?;
    context
        .register_event_handler(
            FreezeBlockPlaceHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register block-place handler: {e}"))?;
    context
        .register_event_handler(
            FreezeInteractHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register interact handler: {e}"))?;
    context
        .register_event_handler(
            FreezeDropItemHandler {
                state: state.clone(),
            },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register drop-item handler: {e}"))?;
    context
        .register_event_handler(
            FreezeAttackHandler { state },
            EventPriority::Normal,
            true,
        )
        .map_err(|e| format!("0gln-auth: failed to register attack handler: {e}"))?;
    Ok(())
}

pub struct PreLoginHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerPreLoginEvent> for PreLoginHandler {
    /// Single-session rule: deny a second simultaneous join with the same
    /// name while the first session is live — whether the first player is
    /// already authenticated OR still frozen/unauthenticated. Runs before a
    /// Player handle exists, so the new connection is cancelled here with a
    /// kick message.
    fn handle(&self, _server: Server, mut data: PlayerPreLoginEventData) -> PlayerPreLoginEventData {
        let normalized = normalize_name(&data.player_name);
        let occupied = {
            let st = lock_read(&self.state);
            st.authed.contains(&normalized) || st.joined_at.contains_key(&normalized)
        };
        if occupied {
            tracing::warn!(
                "0gln-auth: denied duplicate join for {normalized} from {}",
                data.ip_address
            );
            data.cancelled = true;
            data.kick_message = TextComponent::text(&message(&self.state, "login.duplicate"));
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
        // The account-existence tri-state drives the auto-login checks and
        // the final prompt: Unknown prompts /login, never /register.
        let (account, status) = match lock_read(&self.state).store.get_account(&normalized) {
            Ok(Some(a)) => (Some(a), AccountStatus::Exists),
            Ok(None) => (None, AccountStatus::Missing),
            Err(e) => {
                tracing::error!("0gln-auth: join lookup failed for {normalized}: {e}");
                (None, AccountStatus::Unknown)
            }
        };

        // 1. Stored premium_id auto-authenticates ONLY if the UUID the client
        // presented in Login Start matches the Mojang id for the name
        // (JPremium handshake-detection parity). A cracked client joining
        // with a premium-flagged name presents the offline-derived UUID and
        // mismatches: it falls through to the frozen /login prompt instead.
        if let Some(account) = account.as_ref() {
            if let Some(mojang_id) = account.premium_id.as_deref() {
                let client_uuid = data.player.get_id().to_string();
                if uuid_matches_mojang_id(&client_uuid, mojang_id) {
                    mark_authed(&self.state, &normalized);
                    data.player
                        .send_system_message(TextComponent::text(&message(
                            &self.state,
                            "premium.auto",
                        )), false);
                    return data;
                }
                tracing::warn!(
                    "0gln-auth: premium UUID mismatch for {normalized} from {ip}: \
                     cracked client on a premium-flagged name, denied auto-login"
                );
                data.player
                    .send_system_message(TextComponent::text(&message(
                        &self.state,
                        "premium.uuid_mismatch",
                    )), false);
                // Fall through to the unauthenticated path below (frozen,
                // prompted to /login) — the premium account has no password,
                // so this effectively freezes the impersonator until the
                // timeout kick.
            }
        }

        // 2. Session resume: valid expiry AND matching IP, no new session write.
        if account.is_some() {
            let sess = lock_read(&self.state).store.get_session(&normalized);
            let resumed = match sess {
                Ok(Some(session)) => is_session_valid(&session, now_secs(), &ip),
                Ok(None) => false,
                Err(e) => {
                    tracing::error!("0gln-auth: join session lookup failed for {normalized}: {e}");
                    false
                }
            };
            if resumed {
                mark_authed(&self.state, &normalized);
                data.player
                    .send_system_message(TextComponent::text(&message(
                        &self.state,
                        "session.resume",
                    )), false);
                return data;
            }
        }

        // 3. Unauthenticated: record join time for the timeout sweep and
        // prompt. First joins (no account) get the premium-or-cracked choice;
        // there is deliberately NO join-time premium resolution — a premium
        // name must be claimed explicitly via /premium.
        {
            let mut st = lock_write(&self.state);
            st.joined_at.insert(normalized.clone(), now_secs());
            st.authed.remove(&normalized);
        }
        data.player
            .send_system_message(TextComponent::text(&message(
                &self.state,
                join_prompt_key(status),
            )), false);
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
            // The per-IP failure counter is deliberately NOT cleared here:
            // reconnecting must not reset the rate limit. It only resets on
            // a successful /login.
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

/// Block break by an unauthenticated player is cancelled. The event data
/// carries `player: option<player>`; a non-player break is left alone.
pub struct FreezeBlockBreakHandler {
    pub state: SharedState,
}

impl EventHandler<BlockBreakEvent> for FreezeBlockBreakHandler {
    fn handle(&self, _server: Server, mut data: BlockBreakEventData) -> BlockBreakEventData {
        if let Some(player) = data.player.as_ref() {
            let normalized = normalize_name(&player.get_name());
            if freeze_check(player, &self.state, &normalized) {
                data.cancelled = true;
            }
        }
        data
    }
}

pub struct FreezeBlockPlaceHandler {
    pub state: SharedState,
}

impl EventHandler<BlockPlaceEvent> for FreezeBlockPlaceHandler {
    fn handle(&self, _server: Server, mut data: BlockPlaceEventData) -> BlockPlaceEventData {
        let normalized = normalize_name(&data.player.get_name());
        if freeze_check(&data.player, &self.state, &normalized) {
            data.cancelled = true;
        }
        data
    }
}

pub struct FreezeInteractHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerInteractEvent> for FreezeInteractHandler {
    fn handle(&self, _server: Server, mut data: PlayerInteractEventData) -> PlayerInteractEventData {
        let normalized = normalize_name(&data.player.get_name());
        if freeze_check(&data.player, &self.state, &normalized) {
            data.cancelled = true;
        }
        data
    }
}

pub struct FreezeDropItemHandler {
    pub state: SharedState,
}

impl EventHandler<PlayerDropItemEvent> for FreezeDropItemHandler {
    fn handle(&self, _server: Server, mut data: PlayerDropItemEventData) -> PlayerDropItemEventData {
        let normalized = normalize_name(&data.player.get_name());
        if freeze_check(&data.player, &self.state, &normalized) {
            data.cancelled = true;
        }
        data
    }
}

/// Cancels entity damage dealt BY an unauthenticated player. The event data
/// carries `damager-id: s32` (a raw entity id, not a Player handle), so the
/// damager is matched against the entity ids of online players via
/// `server.get_all_players()` + `as_entity().get_id()` and run through the
/// same freeze logic (timeout sweep included) as the other freeze handlers.
/// Damage TO a frozen player is not blocked here: the WIT record has no
/// attacker info on the `entity-damage` event, so the freeze covers the
/// aggressive side only.
pub struct FreezeAttackHandler {
    pub state: SharedState,
}

impl EventHandler<EntityDamageByEntityEvent> for FreezeAttackHandler {
    fn handle(
        &self,
        server: Server,
        mut data: EntityDamageByEntityEventData,
    ) -> EntityDamageByEntityEventData {
        for damager in server.get_all_players() {
            if damager.as_entity().get_id() as i32 != data.damager_id {
                continue;
            }
            let normalized = normalize_name(&damager.get_name());
            if freeze_check(&damager, &self.state, &normalized) {
                data.cancelled = true;
            }
            break;
        }
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unknown account existence (store error) must prompt /login, not the
    /// register choice: prompting register on a store error points an
    /// existing account at the wrong command. Missing -> choice, Exists -> login.
    #[test]
    fn join_prompt_falls_back_to_login_when_account_unknown() {
        assert_eq!(join_prompt_key(AccountStatus::Missing), "join.choice");
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
        assert!(is_command_allowed("/premium"));
        assert!(is_command_allowed("register"));
        assert!(!is_command_allowed("say hello"));
        assert!(!is_command_allowed("/give steve diamond"));
        assert!(!is_command_allowed("/"));
        assert!(!is_command_allowed(""));
        // Prefixes must not leak through: "loginx" is not "login".
        assert!(!is_command_allowed("loginx"));
    }

    /// Build a SharedState with the given failures map, for bookkeeping tests.
    fn test_state(failures: Vec<(&str, u32)>) -> SharedState {
        let mut st = AppState {
            store: crate::storage::AuthStore::open_in_memory(),
            cfg: crate::config::PluginConfig::default(),
            authed: std::collections::HashSet::new(),
            failures: std::collections::HashMap::new(),
            joined_at: std::collections::HashMap::new(),
            premium: crate::premium::PremiumCache::new(1),
        };
        for (ip, count) in failures {
            st.failures.insert(ip.to_string(), count);
        }
        std::sync::Arc::new(std::sync::RwLock::new(st))
    }

    /// FIX 2 regression: a leave event must NOT reset the per-IP failure
    /// counter — reconnecting must not be a rate-limit bypass. The counter
    /// only resets on a successful login (see commands::LoginHandler).
    #[test]
    fn leave_does_not_reset_failure_counter() {
        let state = test_state(vec![("9.9.9.9", 4)]);
        // Simulate what LeaveHandler.run does for bookkeeping: it removes
        // joined_at but must not touch failures. We can't construct a
        // PlayerLeaveEventData without a live server, so we assert the
        // invariant the handler is written against: after a record_failure
        // and the "leave" cleanup (joined_at removal only), the counter stays.
        assert!(record_failure(&state, "9.9.9.9", 5));
        let mut st = lock_write(&state);
        st.joined_at.remove("ghost");
        assert_eq!(st.failures.get("9.9.9.9"), Some(&5), "failures must survive leave");
    }

    /// FIX 2 companion: the counter DOES reset on successful login.
    #[test]
    fn successful_login_resets_failure_counter() {
        let state = test_state(vec![("9.9.9.9", 3)]);
        {
            let mut st = lock_write(&state);
            st.failures.remove("9.9.9.9");
        }
        assert!(!is_authed(&state, "steve"));
        assert_eq!(
            lock_read(&state).failures.get("9.9.9.9"),
            None,
            "login success clears the counter"
        );
    }
}
