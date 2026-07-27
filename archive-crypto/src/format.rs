use crate::RecipientMaterial;
use anyhow::{Context, Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use libsodium_rs::crypto_box;
use libsodium_rs::crypto_secretstream::xchacha20poly1305 as secretstream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroize;

pub const ARCHIVE_FILENAME: &str = "archive.ashe";
const ARCHIVE_MAGIC: &[u8; 8] = b"ASHEARC1";
const ARCHIVE_SCHEMA_VERSION: u32 = 1;
const PAYLOAD_CHUNK_BYTES: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_PAYLOAD_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveHeader {
    pub schema_version: u32,
    pub day: String,
    pub created_at_unix_s: u64,
    pub key_id: String,
    pub recipient: RecipientMaterial,
    pub payload_format: String,
    pub compression: String,
    pub payload_len: usize,
    pub payload_sha256: String,
    pub sealed_data_key_b64: String,
    pub secretstream_header_b64: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveMetadata {
    pub day: String,
    pub key_id: String,
    pub sha256: String,
    pub size: u64,
}

pub struct DecryptedArchive {
    pub header: ArchiveHeader,
    pub payload: Vec<u8>,
}

pub fn encrypt_archive_file(
    day: &str,
    payload: &[u8],
    recipient: &RecipientMaterial,
    path: &Path,
) -> Result<ArchiveMetadata> {
    libsodium_rs::ensure_init().context("failed to initialize libsodium")?;
    validate_day(day)?;
    ensure!(
        payload.len() <= MAX_PAYLOAD_BYTES,
        "archive payload exceeds the supported size"
    );
    recipient.validate_public_material()?;
    let public_key = recipient.public_key()?;
    let data_key = secretstream::Key::generate();
    let sealed_data_key = crypto_box::seal_box(data_key.as_bytes(), &public_key)
        .context("failed to seal archive data key")?;
    let (mut push_state, stream_header) = secretstream::PushState::init_push(&data_key)
        .context("failed to initialize archive encryption")?;
    let header = ArchiveHeader {
        schema_version: ARCHIVE_SCHEMA_VERSION,
        day: day.to_string(),
        created_at_unix_s: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        key_id: recipient.key_id.clone(),
        recipient: recipient.clone(),
        payload_format: "tar".to_string(),
        compression: "zstd".to_string(),
        payload_len: payload.len(),
        payload_sha256: sha256_bytes(payload),
        sealed_data_key_b64: BASE64.encode(sealed_data_key),
        secretstream_header_b64: BASE64.encode(stream_header),
    };
    let header_bytes = serde_json::to_vec(&header).context("failed to serialize archive header")?;
    ensure!(
        header_bytes.len() <= MAX_HEADER_BYTES,
        "archive header is too large"
    );

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("refusing to overwrite {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(ARCHIVE_MAGIC)?;
    write_u32(&mut writer, header_bytes.len())?;
    writer.write_all(&header_bytes)?;

    if payload.is_empty() {
        let ciphertext = push_state
            .push(&[], Some(&header_bytes), secretstream::TAG_FINAL)
            .context("failed to encrypt empty archive payload")?;
        write_chunk(&mut writer, &ciphertext)?;
    } else {
        let chunk_count = payload.len().div_ceil(PAYLOAD_CHUNK_BYTES);
        for (index, chunk) in payload.chunks(PAYLOAD_CHUNK_BYTES).enumerate() {
            let tag = if index + 1 == chunk_count {
                secretstream::TAG_FINAL
            } else {
                secretstream::TAG_MESSAGE
            };
            let aad = (index == 0).then_some(header_bytes.as_slice());
            let ciphertext = push_state
                .push(chunk, aad, tag)
                .context("failed to encrypt archive payload")?;
            write_chunk(&mut writer, &ciphertext)?;
        }
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;

    let verified = decrypt_payload_with_key(path, &data_key)?;
    ensure!(
        verified == payload,
        "archive verification did not reproduce its payload"
    );
    drop(data_key);

    let size = std::fs::metadata(path)?.len();
    Ok(ArchiveMetadata {
        day: day.to_string(),
        key_id: recipient.key_id.clone(),
        sha256: sha256_file(path)?,
        size,
    })
}

pub fn decrypt_archive_file(path: &Path, passphrase: &str) -> Result<DecryptedArchive> {
    libsodium_rs::ensure_init().context("failed to initialize libsodium")?;
    let parsed = read_header(path)?;
    parsed.header.recipient.validate_public_material()?;
    ensure!(
        parsed.header.key_id == parsed.header.recipient.key_id,
        "archive recipient key ID mismatch"
    );
    let keypair = parsed.header.recipient.unlock(passphrase)?;
    let mut sealed_data_key = BASE64
        .decode(&parsed.header.sealed_data_key_b64)
        .context("sealed archive data key is not valid base64")?;
    let mut data_key_bytes =
        crypto_box::open_sealed_box(&sealed_data_key, &keypair.public_key, &keypair.secret_key)
            .context("failed to open archive data key")?;
    sealed_data_key.zeroize();
    ensure!(
        data_key_bytes.len() == secretstream::KEYBYTES,
        "archive data key has invalid length"
    );
    let data_key =
        secretstream::Key::from_bytes(&data_key_bytes).context("archive data key is invalid")?;
    data_key_bytes.zeroize();
    let payload = decrypt_payload_from_parsed(parsed, &data_key)?;
    Ok(DecryptedArchive {
        header: read_header(path)?.header,
        payload,
    })
}

pub fn inspect_archive(path: &Path) -> Result<ArchiveHeader> {
    read_header(path).map(|parsed| parsed.header)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?,
    );
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(digest.finalize().as_slice()))
}

struct ParsedArchive {
    header: ArchiveHeader,
    header_bytes: Vec<u8>,
    reader: BufReader<File>,
}

fn read_header(path: &Path) -> Result<ParsedArchive> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut magic = [0u8; ARCHIVE_MAGIC.len()];
    reader
        .read_exact(&mut magic)
        .context("archive is missing its magic header")?;
    ensure!(&magic == ARCHIVE_MAGIC, "file is not an Ashe archive");
    let header_len = read_u32(&mut reader)?;
    ensure!(
        (1..=MAX_HEADER_BYTES).contains(&header_len),
        "archive header length is invalid"
    );
    let mut header_bytes = vec![0u8; header_len];
    reader
        .read_exact(&mut header_bytes)
        .context("archive header is truncated")?;
    let header: ArchiveHeader =
        serde_json::from_slice(&header_bytes).context("archive header is not valid JSON")?;
    validate_header(&header)?;
    Ok(ParsedArchive {
        header,
        header_bytes,
        reader,
    })
}

fn decrypt_payload_with_key(path: &Path, data_key: &secretstream::Key) -> Result<Vec<u8>> {
    let parsed = read_header(path)?;
    decrypt_payload_from_parsed(parsed, data_key)
}

fn decrypt_payload_from_parsed(
    mut parsed: ParsedArchive,
    data_key: &secretstream::Key,
) -> Result<Vec<u8>> {
    let stream_header_bytes = decode_exact(
        &parsed.header.secretstream_header_b64,
        secretstream::HEADERBYTES,
        "secretstream header",
    )?;
    let stream_header: [u8; secretstream::HEADERBYTES] = stream_header_bytes
        .try_into()
        .map_err(|_| anyhow!("secretstream header has invalid length"))?;
    let mut pull_state = secretstream::PullState::init_pull(&stream_header, data_key)
        .context("failed to initialize archive decryption")?;
    let mut payload = Vec::with_capacity(parsed.header.payload_len);
    let mut first = true;
    let mut final_seen = false;
    while let Some(chunk_len) = read_optional_u32(&mut parsed.reader)? {
        ensure!(!final_seen, "archive contains data after the final chunk");
        ensure!(
            chunk_len <= PAYLOAD_CHUNK_BYTES + secretstream::ABYTES,
            "archive ciphertext chunk is too large"
        );
        let mut ciphertext = vec![0u8; chunk_len];
        parsed
            .reader
            .read_exact(&mut ciphertext)
            .context("archive ciphertext chunk is truncated")?;
        let aad = first.then_some(parsed.header_bytes.as_slice());
        let (plaintext, tag) = pull_state
            .pull(&ciphertext, aad)
            .context("archive authentication failed")?;
        payload.extend_from_slice(&plaintext);
        ensure!(
            payload.len() <= MAX_PAYLOAD_BYTES,
            "archive payload exceeds the supported size"
        );
        final_seen = tag == secretstream::TAG_FINAL;
        first = false;
    }
    ensure!(
        final_seen,
        "archive is missing its final authenticated chunk"
    );
    ensure!(
        payload.len() == parsed.header.payload_len,
        "archive payload length mismatch"
    );
    ensure!(
        sha256_bytes(&payload) == parsed.header.payload_sha256,
        "archive payload checksum mismatch"
    );
    Ok(payload)
}

fn validate_header(header: &ArchiveHeader) -> Result<()> {
    ensure!(
        header.schema_version == ARCHIVE_SCHEMA_VERSION,
        "unsupported archive schema version"
    );
    validate_day(&header.day)?;
    ensure!(
        header.key_id == header.recipient.key_id,
        "archive key ID mismatch"
    );
    ensure!(
        header.payload_format == "tar",
        "unsupported archive payload format"
    );
    ensure!(
        header.compression == "zstd",
        "unsupported archive compression"
    );
    ensure!(
        header.payload_len <= MAX_PAYLOAD_BYTES,
        "archive payload length exceeds the supported size"
    );
    ensure!(
        header.payload_sha256.len() == 64
            && header
                .payload_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "archive payload checksum is invalid"
    );
    let sealed = BASE64
        .decode(&header.sealed_data_key_b64)
        .context("sealed archive data key is not valid base64")?;
    ensure!(
        sealed.len() == secretstream::KEYBYTES + crypto_box::SEALBYTES,
        "sealed archive data key has invalid length"
    );
    decode_exact(
        &header.secretstream_header_b64,
        secretstream::HEADERBYTES,
        "secretstream header",
    )?;
    Ok(())
}

fn validate_day(day: &str) -> Result<()> {
    ensure!(
        day.len() == 10
            && day.as_bytes()[4] == b'-'
            && day.as_bytes()[7] == b'-'
            && day
                .bytes()
                .enumerate()
                .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit()),
        "archive day must use YYYY-MM-DD"
    );
    Ok(())
}

fn write_chunk(writer: &mut impl Write, ciphertext: &[u8]) -> Result<()> {
    write_u32(writer, ciphertext.len())?;
    writer.write_all(ciphertext)?;
    Ok(())
}

fn write_u32(writer: &mut impl Write, value: usize) -> Result<()> {
    let value = u32::try_from(value).context("archive field is too large")?;
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn read_u32(reader: &mut impl Read) -> Result<usize> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes) as usize)
}

fn read_optional_u32(reader: &mut impl Read) -> Result<Option<usize>> {
    let mut bytes = [0u8; 4];
    let read = reader.read(&mut bytes[..1])?;
    if read == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut bytes[1..])?;
    Ok(Some(u32::from_le_bytes(bytes) as usize))
}

fn decode_exact(encoded: &str, length: usize, label: &str) -> Result<Vec<u8>> {
    let bytes = BASE64
        .decode(encoded)
        .with_context(|| format!("{label} is not valid base64"))?;
    ensure!(bytes.len() == length, "{label} has invalid length");
    Ok(bytes)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{decrypt_archive_file, encrypt_archive_file, inspect_archive};
    use crate::RecipientMaterial;
    use libsodium_rs::crypto_pwhash::argon2id;
    use std::fs;

    #[test]
    fn archive_round_trip_is_self_contained_and_tamper_evident() {
        let recipient = RecipientMaterial::create_with_limits(
            "alpha beta gamma delta epsilon",
            argon2id::OPSLIMIT_INTERACTIVE,
            argon2id::MEMLIMIT_INTERACTIVE,
        )
        .unwrap();
        let directory = tempfile_directory("archive-round-trip");
        let path = directory.join("archive.ashe");
        let payload = b"compressed archive payload";
        encrypt_archive_file("2026-07-25", payload, &recipient, &path).unwrap();
        let decrypted = decrypt_archive_file(&path, "alpha beta gamma delta epsilon").unwrap();
        assert_eq!(decrypted.payload, payload);
        assert_eq!(inspect_archive(&path).unwrap().day, "2026-07-25");

        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(decrypt_archive_file(&path, "alpha beta gamma delta epsilon").is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    fn tempfile_directory(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ashe-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }
}
