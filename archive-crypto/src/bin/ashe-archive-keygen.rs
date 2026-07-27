use anyhow::{Context, Result, anyhow, ensure};
use ashe_archive::RecipientMaterial;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

#[cfg(all(target_os = "windows", target_env = "gnu"))]
#[path = "../bin_support/mingw_compat.rs"]
mod mingw_compat;

fn main() -> Result<()> {
    let (passphrase_path, output_path) = parse_args()?;
    let passphrase = read_passphrase(&passphrase_path)?;
    let material = RecipientMaterial::create(passphrase.trim_end())?;
    let restored = material
        .unlock(passphrase.trim_end())
        .context("generated recipient failed its unlock check")?;
    ensure!(
        restored.public_key.as_bytes() == material.public_key()?.as_bytes(),
        "generated recipient failed its public-key check"
    );
    write_new(&output_path, &serde_json::to_vec_pretty(&material)?)?;
    println!(
        "archive recipient created: {} (key_id={})",
        output_path.display(),
        material.key_id
    );
    Ok(())
}

fn parse_args() -> Result<(PathBuf, PathBuf)> {
    let mut args = std::env::args_os().skip(1);
    let mut passphrase = None;
    let mut output = None;
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--passphrase-file" => passphrase = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            _ => return Err(anyhow!("unknown argument")),
        }
    }
    let passphrase = passphrase.context("--passphrase-file is required")?;
    let output = output.context("--output is required")?;
    ensure!(passphrase != output, "input and output paths must differ");
    Ok((passphrase, output))
}

fn read_passphrase(path: &Path) -> Result<Zeroizing<String>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "passphrase path is not a regular file"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let mut passphrase = Zeroizing::new(String::new());
    file.read_to_string(&mut passphrase)
        .context("failed to read passphrase")?;
    ensure!(
        passphrase.split_whitespace().count() == 5,
        "passphrase must contain exactly five words"
    );
    Ok(passphrase)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("refusing to overwrite {}", path.display()))?;
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
