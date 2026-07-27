use anyhow::{Context, Result, anyhow, ensure};
use ashe_archive::{decrypt_archive_file, extract_bundle};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

#[cfg(all(target_os = "windows", target_env = "gnu"))]
#[path = "../bin_support/mingw_compat.rs"]
mod mingw_compat;

fn main() -> Result<()> {
    let arguments = parse_args()?;
    let passphrase = match arguments.passphrase_file {
        Some(path) => read_passphrase(&path)?,
        None => Zeroizing::new(
            rpassword::prompt_password("Five-word archive passphrase: ")
                .context("failed to read archive passphrase")?,
        ),
    };
    ensure!(
        passphrase.split_whitespace().count() == 5,
        "passphrase must contain exactly five words"
    );
    let archive = decrypt_archive_file(&arguments.archive, passphrase.trim())?;
    let day = extract_bundle(&archive.payload, &arguments.output)?;
    println!(
        "restored archive day={} output={}",
        day,
        arguments.output.display()
    );
    Ok(())
}

struct Arguments {
    archive: PathBuf,
    output: PathBuf,
    passphrase_file: Option<PathBuf>,
}

fn parse_args() -> Result<Arguments> {
    let mut args = std::env::args_os().skip(1);
    let archive = args
        .next()
        .map(PathBuf::from)
        .context("archive path is required")?;
    let output = args
        .next()
        .map(PathBuf::from)
        .context("output path is required")?;
    let mut passphrase_file = None;
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--passphrase-file" => passphrase_file = args.next().map(PathBuf::from),
            _ => return Err(anyhow!("unknown argument")),
        }
    }
    ensure!(archive != output, "archive and output paths must differ");
    Ok(Arguments {
        archive,
        output,
        passphrase_file,
    })
}

fn read_passphrase(path: &Path) -> Result<Zeroizing<String>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "passphrase path is not a regular file"
    );
    let mut file = fs::File::open(path)?;
    let mut passphrase = Zeroizing::new(String::new());
    file.read_to_string(&mut passphrase)?;
    Ok(passphrase)
}
