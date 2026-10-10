use crate::error::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{fs::OpenOptions, path::Path, str::FromStr};

pub async fn open(dir: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str("sqlite:")?
        .filename(dir.join("speculum.sqlite"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;
    sqlx::raw_sql(include_str!("schema.sql"))
        .execute(&pool)
        .await?;
    Ok(pool)
}
pub struct SecretKey([u8; 32]);
impl SecretKey {
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join("secret.key");
        let key = match std::fs::read(&path) {
            Ok(bytes) => bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid secret.key"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut key = [0; 32];
                SystemRandom::new()
                    .fill(&mut key)
                    .map_err(|_| anyhow::anyhow!("Random generator failed"))?;
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                use std::io::Write;
                let mut file = options.open(path)?;
                file.write_all(&key)?;
                file.sync_all()?;
                key
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Self(key))
    }
    fn key(&self) -> Result<aead::LessSafeKey> {
        Ok(aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.0)
                .map_err(|_| Error::bad("Invalid encryption key"))?,
        ))
    }
    pub fn encrypt(&self, input: &[u8]) -> Result<String> {
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| Error::bad("Random generator failed"))?;
        let mut bytes = input.to_vec();
        self.key()?
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut bytes,
            )
            .map_err(|_| Error::bad("Encryption failed"))?;
        Ok(STANDARD.encode([nonce.as_slice(), bytes.as_slice()].concat()))
    }
    pub fn decrypt(&self, input: &str) -> Result<Vec<u8>> {
        let mut bytes = STANDARD
            .decode(input)
            .map_err(|_| Error::bad("Invalid encrypted settings"))?;
        if bytes.len() < 28 {
            return Err(Error::bad("Invalid encrypted settings"));
        }
        let nonce: [u8; 12] = bytes[..12].try_into().unwrap();
        let plain = self
            .key()?
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut bytes[12..],
            )
            .map_err(|_| {
                Error::bad("Unable to decrypt settings; restore the matching secret.key")
            })?;
        Ok(plain.to_vec())
    }
}
