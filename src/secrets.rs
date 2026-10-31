//! API keys, kept in the OS keystore (Keychain / Windows Credential Manager),
//! never in config.json.
//!
//! All keys live in one keystore entry as a JSON map, so a launch costs one
//! keystore read. When no keystore is available the map is written to
//! `secrets.bin` with a `plain:` prefix — still better than dropping the key,
//! and said loudly rather than pretended.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::{PROVIDERS, data_dir};

const SERVICE: &str = "VeilPilot";
const ACCOUNT: &str = "api-keys";

pub struct Secrets {
    service: String,
    keys: BTreeMap<String, String>,
    /// Set once the keystore refused us, so later writes go straight to disk.
    plain_fallback: bool,
}

pub fn secrets_path() -> PathBuf {
    data_dir().join("secrets.bin")
}

impl Secrets {
    pub fn load() -> Self {
        // A relocated data dir (tests, a second profile) gets its own keystore
        // entry, so it can never read or overwrite the real keys.
        if std::env::var_os("VEILPILOT_DATA_DIR").is_some() {
            return Self::load_from(&format!("{SERVICE} ({})", data_dir().display()));
        }
        Self::load_from(SERVICE)
    }

    fn load_from(service: &str) -> Self {
        let mut me = Self { service: service.to_string(), keys: BTreeMap::new(), plain_fallback: false };

        // A plain-text file means an earlier run had no keystore; keep using it.
        if let Ok(raw) = std::fs::read(secrets_path()) {
            if let Some(text) = raw.strip_prefix(b"plain:") {
                if let Ok(map) = serde_json::from_slice(text) {
                    me.keys = map;
                    me.plain_fallback = true;
                    return me;
                }
            }
        }

        match keyring::Entry::new(&me.service, ACCOUNT).and_then(|e| e.get_password()) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(map) => me.keys = map,
                Err(err) => eprintln!("[secrets] keystore entry unreadable: {err}"),
            },
            Err(keyring::Error::NoEntry) => {}
            Err(err) => eprintln!("[secrets] keystore unavailable: {err}"),
        }
        me
    }

    pub fn get(&self, provider: &str) -> String {
        self.keys.get(provider).cloned().unwrap_or_default()
    }

    pub fn has(&self, provider: &str) -> bool {
        self.keys.get(provider).is_some_and(|k| !k.is_empty())
    }

    pub fn set(&mut self, provider: &str, key: &str) -> anyhow::Result<()> {
        let key = key.trim();
        if key.is_empty() {
            self.keys.remove(provider);
        } else {
            self.keys.insert(provider.to_string(), key.to_string());
        }
        self.persist()
    }

    /// Which providers have a key, for the settings view.
    pub fn status(&self) -> BTreeMap<&'static str, bool> {
        PROVIDERS.iter().map(|p| (p.id, self.has(p.id))).collect()
    }

    fn persist(&mut self) -> anyhow::Result<()> {
        let text = serde_json::to_string(&self.keys)?;
        if !self.plain_fallback {
            let stored = keyring::Entry::new(&self.service, ACCOUNT).and_then(|entry| {
                if self.keys.is_empty() {
                    match entry.delete_credential() {
                        Err(keyring::Error::NoEntry) => Ok(()),
                        other => other,
                    }
                } else {
                    entry.set_password(&text)
                }
            });
            match stored {
                Ok(()) => return Ok(()),
                Err(err) => {
                    eprintln!(
                        "[secrets] OS keystore unavailable ({err}) — API keys saved unencrypted at {}",
                        secrets_path().display()
                    );
                    self.plain_fallback = true;
                }
            }
        }
        write_plain(&text)
    }

    #[cfg(test)]
    pub fn empty_for_tests() -> Self {
        Self { service: "VeilPilot-test".into(), keys: BTreeMap::new(), plain_fallback: true }
    }

    #[cfg(test)]
    fn wipe(&mut self) {
        self.keys.clear();
        let _ = self.persist();
    }
}

fn write_plain(text: &str) -> anyhow::Result<()> {
    let path = secrets_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = b"plain:".to_vec();
    body.extend_from_slice(text.as_bytes());
    std::fs::write(&path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Talks to the real keystore under a throwaway service name.
    #[test]
    #[ignore = "touches the OS keystore; run with --ignored"]
    fn keystore_round_trip() {
        let service = "VeilPilot-test";
        let mut s = Secrets::load_from(service);
        s.wipe();
        s.set("openai", "  sk-test-123 ").unwrap();
        assert!(!s.plain_fallback, "keystore should be available");
        let again = Secrets::load_from(service);
        assert_eq!(again.get("openai"), "sk-test-123");
        assert!(again.status()["openai"]);
        assert!(!again.status()["anthropic"]);
        let mut again = again;
        again.set("openai", "").unwrap();
        assert!(!Secrets::load_from(service).has("openai"));
    }
}
