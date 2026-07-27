use anyhow::{Context, Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use libsodium_rs::crypto_aead::xchacha20poly1305;
use libsodium_rs::crypto_box;
use libsodium_rs::crypto_pwhash::argon2id;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

mod bundle;
mod format;

pub use bundle::{Bundle, extract_bundle, pack_day};
pub use format::{
    ARCHIVE_FILENAME, ArchiveHeader, ArchiveMetadata, DecryptedArchive, decrypt_archive_file,
    encrypt_archive_file, inspect_archive, sha256_file,
};

pub const RECIPIENT_SCHEMA_VERSION: u32 = 1;
const ENVELOPE_AAD_DOMAIN: &[u8] = b"ashe-archive-private-key-envelope-v1";
const MAX_KDF_OPSLIMIT: u64 = 16;
const MAX_KDF_MEMLIMIT: usize = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateKeyEnvelope {
    pub schema_version: u32,
    pub key_id: String,
    pub public_key_b64: String,
    pub kdf: String,
    pub opslimit: u64,
    pub memlimit: usize,
    pub salt_b64: String,
    pub nonce_b64: String,
    pub encrypted_seed_b64: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientMaterial {
    pub schema_version: u32,
    pub key_id: String,
    pub public_key_b64: String,
    pub envelope_b64: String,
}

impl RecipientMaterial {
    pub fn create(passphrase: &str) -> Result<Self> {
        Self::create_with_limits(
            passphrase,
            argon2id::OPSLIMIT_MODERATE,
            argon2id::MEMLIMIT_MODERATE,
        )
    }

    pub fn create_with_limits(passphrase: &str, opslimit: u64, memlimit: usize) -> Result<Self> {
        libsodium_rs::ensure_init().context("failed to initialize libsodium")?;
        validate_passphrase(passphrase)?;
        validate_kdf_limits(opslimit, memlimit)?;

        let mut seed = vec![0u8; crypto_box::SECRETKEYBYTES];
        libsodium_rs::random::fill_bytes(&mut seed);
        let keypair = crypto_box::KeyPair::from_seed(&seed)
            .context("failed to generate archive recipient")?;
        let public_key_b64 = BASE64.encode(keypair.public_key.as_bytes());
        let key_id = key_id(keypair.public_key.as_bytes());

        let mut salt = vec![0u8; argon2id::SALTBYTES];
        libsodium_rs::random::fill_bytes(&mut salt);
        let mut derived = argon2id::pwhash(
            xchacha20poly1305::KEYBYTES,
            passphrase.as_bytes(),
            &salt,
            opslimit,
            memlimit,
        )
        .context("failed to derive the private-key wrapping key")?;
        let wrapping_key = xchacha20poly1305::Key::from_bytes(&derived)
            .context("invalid private-key wrapping key")?;
        derived.zeroize();
        let nonce = xchacha20poly1305::Nonce::generate();
        let aad = envelope_aad(&key_id, &public_key_b64, opslimit, memlimit);
        let encrypted_seed = xchacha20poly1305::encrypt(&seed, Some(&aad), &nonce, &wrapping_key)
            .context("failed to encrypt the archive private key")?;
        seed.zeroize();

        let envelope = PrivateKeyEnvelope {
            schema_version: RECIPIENT_SCHEMA_VERSION,
            key_id: key_id.clone(),
            public_key_b64: public_key_b64.clone(),
            kdf: "argon2id13".to_string(),
            opslimit,
            memlimit,
            salt_b64: BASE64.encode(salt),
            nonce_b64: BASE64.encode(nonce.as_bytes()),
            encrypted_seed_b64: BASE64.encode(encrypted_seed),
        };
        let envelope_b64 = BASE64.encode(
            serde_json::to_vec(&envelope).context("failed to serialize recipient envelope")?,
        );
        let material = Self {
            schema_version: RECIPIENT_SCHEMA_VERSION,
            key_id,
            public_key_b64,
            envelope_b64,
        };
        material.validate_public_material()?;
        Ok(material)
    }

    pub fn envelope(&self) -> Result<PrivateKeyEnvelope> {
        let bytes = BASE64
            .decode(&self.envelope_b64)
            .context("recipient envelope is not valid base64")?;
        serde_json::from_slice(&bytes).context("recipient envelope is not valid JSON")
    }

    pub fn public_key(&self) -> Result<crypto_box::PublicKey> {
        let bytes = BASE64
            .decode(&self.public_key_b64)
            .context("recipient public key is not valid base64")?;
        crypto_box::PublicKey::from_bytes(&bytes).context("recipient public key has invalid length")
    }

    pub fn validate_public_material(&self) -> Result<()> {
        ensure!(
            self.schema_version == RECIPIENT_SCHEMA_VERSION,
            "unsupported recipient schema version"
        );
        let public_key = self.public_key()?;
        ensure!(
            self.key_id == key_id(public_key.as_bytes()),
            "recipient key ID mismatch"
        );
        let envelope = self.envelope()?;
        ensure!(
            envelope.schema_version == RECIPIENT_SCHEMA_VERSION,
            "unsupported private-key envelope schema version"
        );
        ensure!(
            envelope.key_id == self.key_id,
            "recipient envelope key ID mismatch"
        );
        ensure!(
            envelope.public_key_b64 == self.public_key_b64,
            "recipient envelope public key mismatch"
        );
        validate_envelope_metadata(&envelope)
    }

    pub fn unlock(&self, passphrase: &str) -> Result<crypto_box::KeyPair> {
        libsodium_rs::ensure_init().context("failed to initialize libsodium")?;
        validate_passphrase(passphrase)?;
        self.validate_public_material()?;
        let envelope = self.envelope()?;
        let salt = decode_exact(&envelope.salt_b64, argon2id::SALTBYTES, "Argon2id salt")?;
        let nonce_bytes = decode_exact(
            &envelope.nonce_b64,
            xchacha20poly1305::NPUBBYTES,
            "private-key envelope nonce",
        )?;
        let nonce = xchacha20poly1305::Nonce::try_from_slice(&nonce_bytes)
            .context("invalid private-key envelope nonce")?;
        let ciphertext = BASE64
            .decode(&envelope.encrypted_seed_b64)
            .context("encrypted private key is not valid base64")?;
        let mut derived = argon2id::pwhash(
            xchacha20poly1305::KEYBYTES,
            passphrase.as_bytes(),
            &salt,
            envelope.opslimit,
            envelope.memlimit,
        )
        .context("failed to derive the private-key wrapping key")?;
        let wrapping_key = xchacha20poly1305::Key::from_bytes(&derived)
            .context("invalid private-key wrapping key")?;
        derived.zeroize();
        let aad = envelope_aad(
            &envelope.key_id,
            &envelope.public_key_b64,
            envelope.opslimit,
            envelope.memlimit,
        );
        let mut seed = xchacha20poly1305::decrypt(&ciphertext, Some(&aad), &nonce, &wrapping_key)
            .map_err(|_| {
            anyhow!("passphrase is incorrect or private-key envelope is damaged")
        })?;
        ensure!(
            seed.len() == crypto_box::SECRETKEYBYTES,
            "decrypted private-key seed has invalid length"
        );
        let keypair =
            crypto_box::KeyPair::from_seed(&seed).context("failed to restore archive recipient")?;
        seed.zeroize();
        ensure!(
            BASE64.encode(keypair.public_key.as_bytes()) == self.public_key_b64,
            "private-key envelope does not match its public key"
        );
        Ok(keypair)
    }
}

fn validate_passphrase(passphrase: &str) -> Result<()> {
    ensure!(!passphrase.is_empty(), "archive passphrase is empty");
    ensure!(
        passphrase.as_bytes().len() <= argon2id::PASSWD_MAX,
        "archive passphrase is too long"
    );
    Ok(())
}

fn validate_envelope_metadata(envelope: &PrivateKeyEnvelope) -> Result<()> {
    ensure!(
        envelope.kdf == "argon2id13",
        "unsupported private-key envelope KDF"
    );
    validate_kdf_limits(envelope.opslimit, envelope.memlimit)?;
    decode_exact(&envelope.salt_b64, argon2id::SALTBYTES, "Argon2id salt")?;
    decode_exact(
        &envelope.nonce_b64,
        xchacha20poly1305::NPUBBYTES,
        "private-key envelope nonce",
    )?;
    let ciphertext = BASE64
        .decode(&envelope.encrypted_seed_b64)
        .context("encrypted private key is not valid base64")?;
    ensure!(
        ciphertext.len() == crypto_box::SECRETKEYBYTES + xchacha20poly1305::ABYTES,
        "encrypted private key has invalid length"
    );
    Ok(())
}

fn validate_kdf_limits(opslimit: u64, memlimit: usize) -> Result<()> {
    ensure!(
        (argon2id::OPSLIMIT_MIN..=MAX_KDF_OPSLIMIT).contains(&opslimit),
        "Argon2id operation limit is outside the supported range"
    );
    ensure!(
        (argon2id::MEMLIMIT_MIN..=MAX_KDF_MEMLIMIT).contains(&memlimit),
        "Argon2id memory limit is outside the supported range"
    );
    Ok(())
}

fn envelope_aad(key_id: &str, public_key_b64: &str, opslimit: u64, memlimit: usize) -> Vec<u8> {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        String::from_utf8_lossy(ENVELOPE_AAD_DOMAIN),
        key_id,
        public_key_b64,
        opslimit,
        memlimit,
    )
    .into_bytes()
}

fn decode_exact(encoded: &str, length: usize, label: &str) -> Result<Vec<u8>> {
    let bytes = BASE64
        .decode(encoded)
        .with_context(|| format!("{label} is not valid base64"))?;
    ensure!(bytes.len() == length, "{label} has invalid length");
    Ok(bytes)
}

fn key_id(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::RecipientMaterial;
    use libsodium_rs::crypto_pwhash::argon2id;

    #[test]
    fn recipient_is_self_contained_and_rejects_wrong_passphrase() {
        let recipient = RecipientMaterial::create_with_limits(
            "alpha beta gamma delta epsilon",
            argon2id::OPSLIMIT_INTERACTIVE,
            argon2id::MEMLIMIT_INTERACTIVE,
        )
        .unwrap();
        let keypair = recipient.unlock("alpha beta gamma delta epsilon").unwrap();
        assert_eq!(
            keypair.public_key.as_bytes(),
            recipient.public_key().unwrap().as_bytes()
        );
        assert!(recipient.unlock("wrong passphrase").is_err());
    }
}
