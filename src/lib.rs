mod commands;
mod config;
mod handlers;
mod hash;
mod messages;
mod premium;
mod session;
mod storage;
mod validation;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};

use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
use pumpkin_plugin_api::{Context, Plugin, PluginMetadata};
use tracing::info;

use crate::config::{default_config_toml, load_config};

pub type SharedState = Arc<RwLock<AppState>>;

pub struct AppState {
    pub store: crate::storage::AuthStore,
    pub cfg: crate::config::PluginConfig,
    pub authed: HashSet<String>,
    pub failures: HashMap<String, u32>,
    pub joined_at: HashMap<String, u64>,
    pub premium: crate::premium::PremiumCache,
}

struct GlnAuth;

impl Plugin for GlnAuth {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "gln-auth".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["0glnmc".into()],
            description: "Auth system: premium auto-login, register/login for cracked".into(),
            dependencies: vec![],
            permissions: vec![
                pumpkin_plugin_api::permissions::FS_READ_DATA.into(),
                pumpkin_plugin_api::permissions::FS_WRITE_DATA.into(),
                pumpkin_plugin_api::permissions::HTTP_OUTBOUND.into(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> pumpkin_plugin_api::Result<()> {
        let data_folder = context.get_data_folder();
        std::fs::create_dir_all(&data_folder)
            .map_err(|e| format!("gln-auth: cannot create data folder {data_folder}: {e}"))?;

        let config_path = Path::new(&data_folder).join("config.toml");
        if !config_path.exists() {
            std::fs::write(&config_path, default_config_toml())
                .map_err(|e| format!("gln-auth: cannot write {}: {e}", config_path.display()))?;
            info!(
                "gln-auth: wrote default config to {}",
                config_path.display()
            );
        }
        let config_str = config_path.to_str().ok_or_else(|| {
            format!(
                "gln-auth: config path {} is not valid unicode",
                config_path.display()
            )
        })?;
        let cfg = load_config(config_str)?;

        let store_path = Path::new(&data_folder).join("gln-auth.json");
        let store = crate::storage::AuthStore::open(store_path.to_str().ok_or_else(|| {
            format!(
                "gln-auth: store path {} is not valid unicode",
                store_path.display()
            )
        })?)
        .map_err(|e| format!("gln-auth: failed to open auth store: {e}"))?;

        let state: SharedState = Arc::new(RwLock::new(AppState {
            store,
            cfg: cfg.clone(),
            authed: HashSet::new(),
            failures: HashMap::new(),
            joined_at: HashMap::new(),
            premium: crate::premium::PremiumCache::new(cfg.premium_cache_minutes * 60),
        }));

        handlers::register_handlers(&context, state.clone())?;

        for (command, permission) in commands::build_commands(state.clone()) {
            let default = if permission == commands::ADMIN_PERMISSION {
                PermissionDefault::Op(PermissionLevel::Three)
            } else {
                PermissionDefault::Allow
            };
            context
                .register_permission(&Permission {
                    node: permission.to_string(),
                    description: "gln-auth command permission".to_string(),
                    default,
                    children: vec![],
                })
                .map_err(|e| format!("gln-auth: failed to register permission {permission}: {e}"))?;
            context.register_command(command, permission);
        }

        info!("gln-auth loaded");
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> pumpkin_plugin_api::Result<()> {
        info!("gln-auth unloaded");
        Ok(())
    }
}

pumpkin_plugin_api::register_plugin!(GlnAuth);
