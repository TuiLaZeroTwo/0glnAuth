use pumpkin_plugin_api::{Context, Plugin, PluginMetadata};
use tracing::info;

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
            permissions: vec![],
        }
    }

    fn on_load(&self, _context: Context) -> pumpkin_plugin_api::Result<()> {
        info!("gln-auth loaded");
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> pumpkin_plugin_api::Result<()> {
        info!("gln-auth unloaded");
        Ok(())
    }
}

pumpkin_plugin_api::register_plugin!(GlnAuth);
