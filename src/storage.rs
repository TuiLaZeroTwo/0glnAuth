use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    pub premium_id: Option<String>,
    pub hash: String,
    pub last_ip: String,
    pub last_seen: i64,
    pub created: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub ip: String,
    pub expires_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct StoreData {
    accounts: HashMap<String, Account>,
    sessions: HashMap<String, Session>,
}

pub struct AuthStore {
    path: PathBuf,
    data: Mutex<StoreData>,
}

impl AuthStore {
    /// A store with no backing file, for unit tests only.
    #[cfg(test)]
    pub fn open_in_memory() -> Self {
        Self {
            path: PathBuf::from(":memory:"),
            data: Mutex::new(StoreData::default()),
        }
    }

    pub fn open(path: &str) -> Result<Self, String> {
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create parent dirs: {e}"))?;
            }
        }
        let data = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read store file {}: {e}", path.display()))?;
            serde_json::from_str(&raw)
                .map_err(|e| format!("store file {} is corrupt: {e}", path.display()))?
        } else {
            StoreData::default()
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
        })
    }

    pub fn get_account(&self, name: &str) -> Result<Option<Account>, String> {
        let key = name.to_lowercase();
        let guard = self.lock()?;
        Ok(guard.accounts.get(&key).cloned())
    }

    pub fn save_account(&self, acct: &Account) -> Result<(), String> {
        let mut guard = self.lock()?;
        let key = acct.name.to_lowercase();
        guard.accounts.insert(key, acct.clone());
        self.persist(&guard)
    }

    pub fn delete_account(&self, name: &str) -> Result<(), String> {
        let mut guard = self.lock()?;
        guard.accounts.remove(&name.to_lowercase());
        self.persist(&guard)
    }

    pub fn get_session(&self, name: &str) -> Result<Option<Session>, String> {
        let key = name.to_lowercase();
        let guard = self.lock()?;
        Ok(guard.sessions.get(&key).cloned())
    }

    pub fn set_session(&self, s: &Session) -> Result<(), String> {
        let mut guard = self.lock()?;
        let key = s.name.to_lowercase();
        guard.sessions.insert(key, s.clone());
        self.persist(&guard)
    }

    pub fn clear_session(&self, name: &str) -> Result<(), String> {
        let mut guard = self.lock()?;
        guard.sessions.remove(&name.to_lowercase());
        self.persist(&guard)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, StoreData>, String> {
        self.data.lock().map_err(|_| "store lock poisoned".to_string())
    }

    fn persist(&self, data: &StoreData) -> Result<(), String> {
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| format!("failed to serialize store: {e}"))?;
        atomic_write(&self.path, &json)
    }
}

fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents).map_err(|e| format!("failed to write temp file: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("failed to replace store file: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_and_session_roundtrip() {
        let path = std::env::temp_dir().join(format!("0gln-auth-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = AuthStore::open(path.to_str().unwrap()).expect("open");
        let acct = Account {
            name: "steve".into(),
            uuid: "uuid-1".into(),
            premium_id: None,
            hash: "h".into(),
            last_ip: "1.2.3.4".into(),
            last_seen: 1,
            created: 1,
        };
        store.save_account(&acct).expect("save");
        assert_eq!(store.get_account("STEVE").unwrap().unwrap().uuid, "uuid-1");
        store
            .set_session(&Session {
                name: "steve".into(),
                ip: "1.2.3.4".into(),
                expires_at: 999,
            })
            .unwrap();
        assert_eq!(store.get_session("steve").unwrap().unwrap().ip, "1.2.3.4");
        store.clear_session("steve").unwrap();
        assert!(store.get_session("steve").unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn temp_corrupt_file_fails_closed() {
        let path = std::env::temp_dir().join(format!("0gln-auth-corrupt-{}.json", std::process::id()));
        std::fs::write(&path, "{ not valid json !!").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(AuthStore::open(path.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_file(&path);
    }
}
