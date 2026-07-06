//! Age-encrypted secrets vault.
//!
//! One file per user, at `<config-dir>/aptui/vault.age`, holding all secret
//! variable values across all environments. Encrypted with a passphrase-
//! derived key using the [age](https://age-encryption.org/) format
//! (ChaCha20-Poly1305).
//!
//! ## Threat model
//!
//! - Attacker with disk access but no passphrase: cannot read secrets.
//! - Attacker who observes the app while unlocked: can read everything.
//! - User who forgets passphrase: data is gone. There is no recovery.
//!
//! ## Layout
//!
//! `VaultData` is a `BTreeMap<String, String>` keyed by `<envName>/<varName>`.
//! Serialized to JSON, then encrypted, then written atomically via tmp+rename.
//!
//! ## State machine
//!
//! `VaultState` sits inside `AppState` on the Tauri host:
//! - `password: None` → locked. Frontend must call `vault_unlock` first.
//! - `password: Some(_)` → unlocked. `cache: Option<VaultData>` populated
//!   lazily on first read to avoid re-decrypting on every access.
//! - `vault_lock` clears both fields, forcing re-prompt on next read.

use age::secrecy::SecretString;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct VaultData {
    // key format: <envName>/<varName>
    pub secrets: BTreeMap<String, String>,
}

pub struct VaultState {
    pub password: Mutex<Option<SecretString>>,
    pub cache: Mutex<Option<VaultData>>,
}

impl Default for VaultState {
    fn default() -> Self {
        Self {
            password: Mutex::new(None),
            cache: Mutex::new(None),
        }
    }
}

fn vault_path() -> Result<PathBuf, String> {
    let bd = BaseDirs::new().ok_or_else(|| "no base dirs".to_string())?;
    let dir = bd.config_dir().join("aptui");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("vault.age"))
}

pub fn vault_exists() -> bool {
    vault_path().map(|p| p.exists()).unwrap_or(false)
}

/// Decrypt and parse the vault file. Returns an empty `VaultData` if the
/// file doesn't exist yet (first-run state). Wrong password returns an error.
pub fn read_encrypted(password: &SecretString) -> Result<VaultData, String> {
    let path = vault_path()?;
    if !path.exists() {
        return Ok(VaultData::default());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let dec = age::Decryptor::new(&bytes[..]).map_err(|e| format!("decrypt init: {}", e))?;
    let identity = age::scrypt::Identity::new(password.clone());
    let mut reader = dec
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| format!("wrong password or corrupt vault: {}", e))?;
    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .map_err(|e| e.to_string())?;
    if plaintext.is_empty() {
        return Ok(VaultData::default());
    }
    serde_json::from_slice(&plaintext).map_err(|e| format!("parse vault: {}", e))
}

/// Encrypt and write the vault. Uses tmp+rename for atomicity — a crash
/// mid-write cannot leave a partially-written vault.
pub fn write_encrypted(password: &SecretString, data: &VaultData) -> Result<(), String> {
    let path = vault_path()?;
    let plaintext = serde_json::to_vec(data).map_err(|e| e.to_string())?;
    let encryptor = age::Encryptor::with_user_passphrase(password.clone());
    let mut buf = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut buf)
        .map_err(|e| format!("encrypt init: {}", e))?;
    writer.write_all(&plaintext).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;

    let tmp = path.with_extension("age.tmp");
    std::fs::write(&tmp, &buf).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn key(env: &str, name: &str) -> String {
    format!("{}/{}", env, name)
}

pub fn get_from(data: &VaultData, env: &str, name: &str) -> Option<String> {
    data.secrets.get(&key(env, name)).cloned()
}

pub fn set_in(data: &mut VaultData, env: &str, name: &str, value: &str) {
    data.secrets.insert(key(env, name), value.to_string());
}

pub fn delete_in(data: &mut VaultData, env: &str, name: &str) {
    data.secrets.remove(&key(env, name));
}

/// Migrate all vault entries from `<old_env>/*` to `<new_env>/*` in-place.
/// Called during env rename so secrets follow their env.
pub fn rename_env_in(data: &mut VaultData, old_env: &str, new_env: &str) {
    let old_prefix = format!("{}/", old_env);
    let new_prefix = format!("{}/", new_env);
    let keys: Vec<String> = data
        .secrets
        .keys()
        .filter(|k| k.starts_with(&old_prefix))
        .cloned()
        .collect();
    for k in keys {
        if let Some(v) = data.secrets.remove(&k) {
            let suffix = &k[old_prefix.len()..];
            data.secrets.insert(format!("{}{}", new_prefix, suffix), v);
        }
    }
}

pub fn delete_env_in(data: &mut VaultData, env: &str) {
    let prefix = format!("{}/", env);
    data.secrets.retain(|k, _| !k.starts_with(&prefix));
}
