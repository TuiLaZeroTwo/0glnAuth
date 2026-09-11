//! Auth commands: register/login/logout/changepassword/unregister + admin setpremium/forcelogin.
//!
//! Fail-closed rules enforced here:
//! - hash/save/session failures send an error and never mark the player authed,
//! - wrong passwords increment the per-IP failure counter and never mark authed,
//! - storage lookup failures abort the action with a generic error (state unchanged).

use pumpkin_plugin_api::command::{
    Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs, StringType,
};
use pumpkin_plugin_api::commands::CommandHandler;
use pumpkin_plugin_api::text::TextComponent;
use pumpkin_plugin_api::{Player, Server};
use std::sync::{RwLockReadGuard, RwLockWriteGuard};

use crate::config::PluginConfig;
use crate::hash::{hash_password, verify_password};
use crate::messages::msg;
use crate::session::{new_expiry, now_secs};
use crate::storage::{Account, Session};
use crate::validation::{normalize_name, validate_name, validate_password};
use crate::{AppState, SharedState};

/// Permission node for player-facing auth commands (registered with default Allow).
pub const PLAYER_PERMISSION: &str = "glnauth.player";
/// Permission node for admin commands (registered with default op-level 3+).
pub const ADMIN_PERMISSION: &str = "glnauth.admin";

const PLAYER_ONLY: &str = "This command can only be used by players.";
const STORE_ERROR: &str = "Internal error, action was not completed.";

fn get_string(args: &ConsumedArgs, key: &str) -> Option<String> {
    match args.get_value(key) {
        Arg::Simple(value) | Arg::Msg(value) => Some(value),
        _ => None,
    }
}

fn reply_ok(sender: &CommandSender, text: &str) {
    sender.send_message(TextComponent::text(text));
}

fn reply_err(sender: &CommandSender, text: &str) {
    sender.send_error(TextComponent::text(text));
}

fn require_player(sender: &CommandSender) -> Option<Player> {
    match sender.as_player() {
        Some(player) => Some(player),
        None => {
            reply_err(sender, PLAYER_ONLY);
            None
        }
    }
}

fn lock_read(state: &SharedState) -> RwLockReadGuard<'_, AppState> {
    state.read().unwrap_or_else(|e| e.into_inner())
}

fn lock_write(state: &SharedState) -> RwLockWriteGuard<'_, AppState> {
    state.write().unwrap_or_else(|e| e.into_inner())
}

fn cfg_of(state: &SharedState) -> PluginConfig {
    lock_read(state).cfg.clone()
}

fn find_account(state: &SharedState, normalized: &str) -> Result<Option<Account>, String> {
    lock_read(state).store.get_account(normalized)
}

fn lookup_or_report(
    state: &SharedState,
    sender: &CommandSender,
    normalized: &str,
) -> Option<Account> {
    match find_account(state, normalized) {
        Ok(Some(account)) => Some(account),
        Ok(None) => {
            reply_err(sender, "Unknown account.");
            None
        }
        Err(e) => {
            tracing::error!("gln-auth: account lookup failed for {normalized}: {e}");
            reply_err(sender, STORE_ERROR);
            None
        }
    }
}

pub fn build_commands(state: SharedState) -> Vec<(Command, &'static str)> {
    vec![
        (
            Command::new(&["register".to_string()], "Register an account")
                .then(
                    CommandNode::argument("password", &ArgumentType::String(StringType::Quotable))
                        .then(
                            CommandNode::argument(
                                "confirm",
                                &ArgumentType::String(StringType::Quotable),
                            )
                            .execute(RegisterHandler { state: state.clone() }),
                        ),
                ),
            PLAYER_PERMISSION,
        ),
        (
            Command::new(
                &["login".to_string(), "l".to_string(), "log".to_string()],
                "Login to your account",
            )
            .then(
                CommandNode::argument("password", &ArgumentType::String(StringType::Quotable))
                    .execute(LoginHandler { state: state.clone() }),
            ),
            PLAYER_PERMISSION,
        ),
        (
            Command::new(&["logout".to_string()], "Logout of your account")
                .execute(LogoutHandler { state: state.clone() }),
            PLAYER_PERMISSION,
        ),
        (
            Command::new(&["changepassword".to_string()], "Change your password").then(
                CommandNode::argument("old", &ArgumentType::String(StringType::Quotable)).then(
                    CommandNode::argument("new", &ArgumentType::String(StringType::Quotable))
                        .execute(ChangePasswordHandler { state: state.clone() }),
                ),
            ),
            PLAYER_PERMISSION,
        ),
        (
            Command::new(&["unregister".to_string()], "Delete your account").then(
                CommandNode::argument("password", &ArgumentType::String(StringType::Quotable))
                    .execute(UnregisterHandler { state: state.clone() }),
            ),
            PLAYER_PERMISSION,
        ),
        (
            Command::new(&["setpremium".to_string()], "Toggle the premium flag of an account")
                .then(
                    CommandNode::argument("name", &ArgumentType::String(StringType::SingleWord))
                        .then(
                            CommandNode::literal("on").execute(SetPremiumHandler {
                                state: state.clone(),
                                enable: true,
                            }),
                        )
                        .then(
                            CommandNode::literal("off").execute(SetPremiumHandler {
                                state: state.clone(),
                                enable: false,
                            }),
                        ),
                ),
            ADMIN_PERMISSION,
        ),
        (
            Command::new(&["forcelogin".to_string()], "Force-login a registered account").then(
                CommandNode::argument("name", &ArgumentType::String(StringType::SingleWord))
                    .execute(ForceLoginHandler { state: state.clone() }),
            ),
            ADMIN_PERMISSION,
        ),
    ]
}

struct RegisterHandler {
    state: SharedState,
}

impl CommandHandler for RegisterHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = require_player(&sender) else {
            return Ok(1);
        };
        let name = player.get_name();
        let uuid = player.get_id().to_string();
        let ip = player.get_ip();
        let normalized = normalize_name(&name);
        let (Some(password), Some(confirm)) =
            (get_string(&args, "password"), get_string(&args, "confirm"))
        else {
            reply_err(&sender, "Usage: /register <password> <confirm>");
            return Ok(1);
        };

        if lock_read(&self.state).authed.contains(&normalized) {
            reply_err(&sender, "You are already logged in.");
            return Ok(1);
        }

        match find_account(&self.state, &normalized) {
            Ok(Some(_)) => {
                reply_err(&sender, "An account with this name already exists.");
                return Ok(1);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("gln-auth: register lookup failed for {normalized}: {e}");
                reply_err(&sender, STORE_ERROR);
                return Ok(1);
            }
        }

        if let Err(e) = validate_name(&name) {
            reply_err(&sender, &e);
            return Ok(1);
        }

        let cfg = cfg_of(&self.state);
        if let Err(e) = validate_password(&password, &normalized, &cfg) {
            reply_err(&sender, &e);
            return Ok(1);
        }
        if password != confirm {
            reply_err(&sender, msg("register.mismatch"));
            return Ok(1);
        }

        let hash = match hash_password(&password) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("gln-auth: register hash failed for {normalized}: {e}");
                reply_err(&sender, "Registration failed, please try again.");
                return Ok(1);
            }
        };

        let now = now_secs();
        let account = Account {
            name,
            uuid,
            premium_id: None,
            hash,
            last_ip: ip.clone(),
            last_seen: now as i64,
            created: now as i64,
        };
        if let Err(e) = lock_read(&self.state).store.save_account(&account) {
            tracing::error!("gln-auth: register save_account failed for {normalized}: {e}");
            reply_err(&sender, STORE_ERROR);
            return Ok(1);
        }

        let session = Session {
            name: account.name.clone(),
            ip,
            expires_at: new_expiry(now, &cfg),
        };
        if let Err(e) = lock_read(&self.state).store.set_session(&session) {
            tracing::error!("gln-auth: register set_session failed for {normalized}: {e}");
            reply_err(&sender, STORE_ERROR);
            return Ok(1);
        }

        lock_write(&self.state).authed.insert(normalized);
        reply_ok(&sender, msg("register.ok"));
        Ok(0)
    }
}

struct LoginHandler {
    state: SharedState,
}

impl CommandHandler for LoginHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = require_player(&sender) else {
            return Ok(1);
        };
        let name = player.get_name();
        let ip = player.get_ip();
        let normalized = normalize_name(&name);
        let Some(password) = get_string(&args, "password") else {
            reply_err(&sender, "Usage: /login <password>");
            return Ok(1);
        };

        let account = match find_account(&self.state, &normalized) {
            Ok(Some(account)) => account,
            Ok(None) => {
                reply_err(&sender, "You are not registered.");
                return Ok(1);
            }
            Err(e) => {
                tracing::error!("gln-auth: login lookup failed for {normalized}: {e}");
                reply_err(&sender, STORE_ERROR);
                return Ok(1);
            }
        };

        if !verify_password(&account.hash, &password) {
            reply_err(&sender, msg("login.wrong"));
            let cfg = cfg_of(&self.state);
            if crate::handlers::record_failure(&self.state, &ip, cfg.max_login_tries) {
                tracing::warn!("gln-auth: ip {ip} kicked after {} failed logins", cfg.max_login_tries);
                crate::handlers::kick_rate_limited(&player, &self.state, &normalized);
            }
            return Ok(1);
        }

        let cfg = cfg_of(&self.state);
        let session = Session {
            name,
            ip: ip.clone(),
            expires_at: new_expiry(now_secs(), &cfg),
        };
        if let Err(e) = lock_read(&self.state).store.set_session(&session) {
            tracing::error!("gln-auth: login set_session failed for {normalized}: {e}");
            reply_err(&sender, STORE_ERROR);
            return Ok(1);
        }

        {
            let mut st = lock_write(&self.state);
            st.authed.insert(normalized);
            st.failures.remove(&ip);
        }
        reply_ok(&sender, msg("login.ok"));
        Ok(0)
    }
}

struct LogoutHandler {
    state: SharedState,
}

impl CommandHandler for LogoutHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = require_player(&sender) else {
            return Ok(1);
        };
        let normalized = normalize_name(&player.get_name());

        let mut st = lock_write(&self.state);
        if !st.authed.contains(&normalized) {
            drop(st);
            reply_err(&sender, "You are not logged in.");
            return Ok(1);
        }
        if let Err(e) = st.store.clear_session(&normalized) {
            tracing::error!("gln-auth: logout clear_session failed for {normalized}: {e}");
            drop(st);
            reply_err(&sender, STORE_ERROR);
            return Ok(1);
        }
        st.authed.remove(&normalized);
        drop(st);

        reply_ok(&sender, "Logged out.");
        Ok(0)
    }
}

struct ChangePasswordHandler {
    state: SharedState,
}

impl CommandHandler for ChangePasswordHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = require_player(&sender) else {
            return Ok(1);
        };
        let name = player.get_name();
        let normalized = normalize_name(&name);
        let (Some(old), Some(new)) = (get_string(&args, "old"), get_string(&args, "new")) else {
            reply_err(&sender, "Usage: /changepassword <old> <new>");
            return Ok(1);
        };

        if !lock_read(&self.state).authed.contains(&normalized) {
            reply_err(&sender, "You must be logged in to change your password.");
            return Ok(1);
        }

        let account = match lookup_or_report(&self.state, &sender, &normalized) {
            Some(account) => account,
            None => return Ok(1),
        };

        if !verify_password(&account.hash, &old) {
            reply_err(&sender, msg("login.wrong"));
            return Ok(1);
        }

        let cfg = cfg_of(&self.state);
        if let Err(e) = validate_password(&new, &normalized, &cfg) {
            reply_err(&sender, &e);
            return Ok(1);
        }

        let hash = match hash_password(&new) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("gln-auth: changepassword hash failed for {normalized}: {e}");
                reply_err(&sender, "Password change failed, please try again.");
                return Ok(1);
            }
        };

        let updated = Account { hash, ..account };
        if let Err(e) = lock_read(&self.state).store.save_account(&updated) {
            tracing::error!("gln-auth: changepassword save failed for {normalized}: {e}");
            reply_err(&sender, STORE_ERROR);
            return Ok(1);
        }

        reply_ok(&sender, "Password changed.");
        Ok(0)
    }
}

struct UnregisterHandler {
    state: SharedState,
}

impl CommandHandler for UnregisterHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = require_player(&sender) else {
            return Ok(1);
        };
        let name = player.get_name();
        let normalized = normalize_name(&name);
        let Some(password) = get_string(&args, "password") else {
            reply_err(&sender, "Usage: /unregister <password>");
            return Ok(1);
        };

        if !lock_read(&self.state).authed.contains(&normalized) {
            reply_err(&sender, "You must be logged in to delete your account.");
            return Ok(1);
        }

        let account = match lookup_or_report(&self.state, &sender, &normalized) {
            Some(account) => account,
            None => return Ok(1),
        };

        if !verify_password(&account.hash, &password) {
            reply_err(&sender, msg("login.wrong"));
            return Ok(1);
        }

        if let Err(e) = lock_read(&self.state).store.delete_account(&normalized) {
            tracing::error!("gln-auth: unregister delete_account failed for {normalized}: {e}");
            reply_err(&sender, STORE_ERROR);
            return Ok(1);
        }

        if let Err(e) = lock_read(&self.state).store.clear_session(&normalized) {
            tracing::warn!("gln-auth: unregister clear_session failed for {normalized}: {e}");
        }
        lock_write(&self.state).authed.remove(&normalized);

        reply_ok(&sender, "Account deleted.");
        Ok(0)
    }
}

struct SetPremiumHandler {
    state: SharedState,
    enable: bool,
}

impl CommandHandler for SetPremiumHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(target) = get_string(&args, "name") else {
            reply_err(&sender, "Usage: /setpremium <name> <on|off>");
            return Ok(1);
        };
        let normalized = normalize_name(&target);

        let mut account = match lookup_or_report(&self.state, &sender, &normalized) {
            Some(account) => account,
            None => return Ok(1),
        };

        account.premium_id = if self.enable {
            Some(account.uuid.clone())
        } else {
            None
        };
        if let Err(e) = lock_read(&self.state).store.save_account(&account) {
            tracing::error!("gln-auth: setpremium save failed for {normalized}: {e}");
            reply_err(&sender, STORE_ERROR);
            return Ok(1);
        }

        let display = account.name.clone();
        if self.enable {
            reply_ok(&sender, &format!("Premium enabled for {display}."));
        } else {
            reply_ok(&sender, &format!("Premium disabled for {display}."));
        }
        Ok(0)
    }
}

struct ForceLoginHandler {
    state: SharedState,
}

impl CommandHandler for ForceLoginHandler {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(target) = get_string(&args, "name") else {
            reply_err(&sender, "Usage: /forcelogin <name>");
            return Ok(1);
        };
        let normalized = normalize_name(&target);

        match lookup_or_report(&self.state, &sender, &normalized) {
            Some(_) => {}
            None => return Ok(1),
        }

        lock_write(&self.state).authed.insert(normalized);
        reply_ok(&sender, &format!("Force-logged in {target}."));
        Ok(0)
    }
}
