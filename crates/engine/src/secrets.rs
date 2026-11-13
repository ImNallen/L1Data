use std::sync::{Arc, Mutex, MutexGuard};

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use crate::model::ConnectionId;

const KEYCHAIN_SERVICE: &str = "dev.l1data.app";
const KEYCHAIN_USER: &str = "master-key";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SecretError(pub String);

/// Holds the one master key that seals every saved password. The sealed passwords live in SQLite
/// rather than as one keychain blob because Windows Credential Manager caps a blob at 2560 bytes.
pub trait KeyStore: Send + Sync {
    fn load(&self) -> Result<Option<[u8; 32]>, SecretError>;
    fn save(&self, key: &[u8; 32]) -> Result<(), SecretError>;
}

#[derive(Debug, Default)]
pub struct KeyringKeyStore;

impl KeyringKeyStore {
    fn entry() -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(|e| SecretError(e.to_string()))
    }
}

impl KeyStore for KeyringKeyStore {
    fn load(&self) -> Result<Option<[u8; 32]>, SecretError> {
        match Self::entry()?.get_secret() {
            Ok(bytes) => bytes
                .try_into()
                .map(Some)
                .map_err(|b: Vec<u8>| SecretError(format!("master key is {} bytes, expected 32", b.len()))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError(e.to_string())),
        }
    }

    fn save(&self, key: &[u8; 32]) -> Result<(), SecretError> {
        Self::entry()?.set_secret(key).map_err(|e| SecretError(e.to_string()))
    }
}

#[derive(Debug, Default)]
pub struct MemoryKeyStore(Mutex<Option<[u8; 32]>>);

impl KeyStore for MemoryKeyStore {
    fn load(&self) -> Result<Option<[u8; 32]>, SecretError> {
        Ok(*self.0.lock().unwrap())
    }

    fn save(&self, key: &[u8; 32]) -> Result<(), SecretError> {
        *self.0.lock().unwrap() = Some(*key);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

/// Which secret of a connection a sealed blob holds. Each slot is sealed with its own associated
/// data, so a blob moved between slots does not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSlot {
    Database,
    Ssh,
}

impl SecretSlot {
    pub fn table(self) -> &'static str {
        match self {
            SecretSlot::Database => "secrets",
            SecretSlot::Ssh => "ssh_secrets",
        }
    }

    fn aad(self, id: ConnectionId) -> String {
        match self {
            SecretSlot::Database => id.0.to_string(),
            SecretSlot::Ssh => format!("{}/ssh", id.0),
        }
    }
}

pub struct Vault {
    keys: Arc<dyn KeyStore>,
    key: Mutex<Option<Key>>,
}

impl Vault {
    pub fn new(keys: Arc<dyn KeyStore>) -> Self {
        Self { keys, key: Mutex::new(None) }
    }

    fn loaded(&self) -> Result<MutexGuard<'_, Option<Key>>, SecretError> {
        let mut key = self.key.lock().unwrap_or_else(|e| e.into_inner());
        if key.is_none() {
            *key = self.keys.load()?.map(Key::from);
        }
        Ok(key)
    }

    pub fn seal(&self, id: ConnectionId, slot: SecretSlot, password: &str) -> Result<Sealed, SecretError> {
        let mut loaded = self.loaded()?;
        let key = match *loaded {
            Some(key) => key,
            None => {
                let key = XChaCha20Poly1305::generate_key(&mut OsRng);
                self.keys.save(&key.into())?;
                *loaded.insert(key)
            }
        };
        drop(loaded);
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let aad = slot.aad(id);
        let ciphertext = XChaCha20Poly1305::new(&key)
            .encrypt(&nonce, Payload { msg: password.as_bytes(), aad: aad.as_bytes() })
            .map_err(|e| SecretError(e.to_string()))?;
        Ok(Sealed { nonce: nonce.into(), ciphertext })
    }

    /// `None` when the password cannot be recovered, so the caller asks for it again. Never creates
    /// a key: a missing one means it was lost (wiped keychain, copied data dir, memory store restart).
    pub fn open(&self, id: ConnectionId, slot: SecretSlot, sealed: &Sealed) -> Result<Option<String>, SecretError> {
        let Some(key) = *self.loaded()? else {
            return Ok(None);
        };
        let aad = slot.aad(id);
        let plain = XChaCha20Poly1305::new(&key)
            .decrypt(XNonce::from_slice(&sealed.nonce), Payload { msg: &sealed.ciphertext, aad: aad.as_bytes() })
            .ok();
        Ok(plain.and_then(|p| String::from_utf8(p).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingKeys {
        inner: MemoryKeyStore,
        loads: AtomicUsize,
        saves: AtomicUsize,
    }

    impl KeyStore for CountingKeys {
        fn load(&self) -> Result<Option<[u8; 32]>, SecretError> {
            self.loads.fetch_add(1, Ordering::Relaxed);
            self.inner.load()
        }

        fn save(&self, key: &[u8; 32]) -> Result<(), SecretError> {
            self.saves.fetch_add(1, Ordering::Relaxed);
            self.inner.save(key)
        }
    }

    fn id() -> ConnectionId {
        ConnectionId(uuid::Uuid::new_v4())
    }

    #[test]
    fn seal_then_open_returns_the_password() {
        let vault = Vault::new(Arc::new(MemoryKeyStore::default()));
        let id = id();
        let sealed = vault.seal(id, SecretSlot::Database, "hunter2").unwrap();
        assert!(!sealed.ciphertext.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(vault.open(id, SecretSlot::Database, &sealed).unwrap().as_deref(), Some("hunter2"));
    }

    #[test]
    fn a_reopened_vault_on_the_same_key_store_opens_the_password() {
        let keys = Arc::new(MemoryKeyStore::default());
        let id = id();
        let sealed = Vault::new(keys.clone()).seal(id, SecretSlot::Database, "hunter2").unwrap();
        assert_eq!(Vault::new(keys).open(id, SecretSlot::Database, &sealed).unwrap().as_deref(), Some("hunter2"));
    }

    #[test]
    fn the_key_is_read_from_the_os_store_once_per_vault() {
        let keys = Arc::new(CountingKeys::default());
        let sealed: Vec<_> = (0..3)
            .map(|_| {
                let id = id();
                (id, Vault::new(keys.clone()).seal(id, SecretSlot::Database, "pw").unwrap())
            })
            .collect();
        let loads_before = keys.loads.load(Ordering::Relaxed);
        let vault = Vault::new(keys.clone());
        for _ in 0..4 {
            for (id, s) in &sealed {
                assert_eq!(vault.open(*id, SecretSlot::Database, s).unwrap().as_deref(), Some("pw"));
            }
        }
        assert_eq!(keys.loads.load(Ordering::Relaxed) - loads_before, 1);
    }

    #[test]
    fn a_password_sealed_for_another_connection_does_not_open() {
        let vault = Vault::new(Arc::new(MemoryKeyStore::default()));
        let sealed = vault.seal(id(), SecretSlot::Database, "hunter2").unwrap();
        assert_eq!(vault.open(id(), SecretSlot::Database, &sealed).unwrap(), None);
    }

    #[test]
    fn a_secret_sealed_in_one_slot_does_not_open_in_the_other() {
        let vault = Vault::new(Arc::new(MemoryKeyStore::default()));
        let id = id();
        let password = vault.seal(id, SecretSlot::Database, "hunter2").unwrap();
        let passphrase = vault.seal(id, SecretSlot::Ssh, "l1data").unwrap();
        assert_eq!(vault.open(id, SecretSlot::Ssh, &password).unwrap(), None);
        assert_eq!(vault.open(id, SecretSlot::Database, &passphrase).unwrap(), None);
        assert_eq!(vault.open(id, SecretSlot::Ssh, &passphrase).unwrap().as_deref(), Some("l1data"));
    }

    #[test]
    fn a_password_sealed_before_slots_existed_still_opens() {
        let keys = Arc::new(MemoryKeyStore::default());
        let vault = Vault::new(keys.clone());
        let id = id();
        vault.seal(id, SecretSlot::Ssh, "creates the key").unwrap();
        let key = Key::from(keys.load().unwrap().unwrap());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let aad = id.0.to_string();
        let ciphertext =
            XChaCha20Poly1305::new(&key).encrypt(&nonce, Payload { msg: b"hunter2", aad: aad.as_bytes() }).unwrap();
        let sealed = Sealed { nonce: nonce.into(), ciphertext };
        assert_eq!(vault.open(id, SecretSlot::Database, &sealed).unwrap().as_deref(), Some("hunter2"));
    }

    #[test]
    fn a_lost_key_opens_nothing_and_creates_no_key() {
        let id = id();
        let sealed = Vault::new(Arc::new(MemoryKeyStore::default())).seal(id, SecretSlot::Database, "hunter2").unwrap();
        let keys = Arc::new(CountingKeys::default());
        assert_eq!(Vault::new(keys.clone()).open(id, SecretSlot::Database, &sealed).unwrap(), None);
        assert_eq!(keys.saves.load(Ordering::Relaxed), 0);
        assert_eq!(keys.load().unwrap(), None);
    }

    #[test]
    fn concurrent_first_seals_share_one_key() {
        let keys = Arc::new(CountingKeys::default());
        let vault = Arc::new(Vault::new(keys.clone()));
        let sealed: Vec<_> = (0..8)
            .map(|_| {
                let vault = vault.clone();
                std::thread::spawn(move || {
                    let id = id();
                    (id, vault.seal(id, SecretSlot::Database, "pw").unwrap())
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        assert_eq!(keys.saves.load(Ordering::Relaxed), 1);
        let fresh = Vault::new(keys.clone());
        for (id, s) in &sealed {
            assert_eq!(fresh.open(*id, SecretSlot::Database, s).unwrap().as_deref(), Some("pw"));
        }
    }
}
