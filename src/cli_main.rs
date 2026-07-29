use anyhow::{Context, Result, anyhow, ensure};
use ashe_archive_crypto::{decrypt_archive_file, extract_bundle};
use ashe_worker::archive::{self, SealDayOutcome};
use ashe_worker::config::AppConfig;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const HELP: &str = r#"Ashe Worker command-line tools

Usage:
  ashe-worker-cli [--artifacts-dir <path>] archive list-uploaded
  ashe-worker-cli [--artifacts-dir <path>] archive seal <YYYY-MM-DD>
  ashe-worker-cli archive upload <archive>
  ashe-worker-cli archive decrypt <archive> <output-directory> [--passphrase-file <path>]
  ashe-worker-cli --help

Commands:
  archive list-uploaded  List archives currently stored by the configured receiver
  archive seal           Generate/check daily.md, encrypt a closed day, then remove plaintext
  archive upload         Upload an existing encrypted archive unchanged
  archive decrypt        Decrypt and restore an archive into a new directory

Configuration is loaded from .env.local using the same rules as ashe-worker.exe.
"#;

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1).collect()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run(arguments: Vec<OsString>) -> Result<()> {
    let command = parse_args(arguments)?;
    match command {
        Command::Help => {
            print!("{HELP}");
            Ok(())
        }
        Command::ListUploaded { artifacts_dir } => {
            let config = load_config(artifacts_dir);
            list_uploaded(&config)
        }
        Command::Seal { artifacts_dir, day } => {
            let config = load_config(artifacts_dir);
            seal_day(&config, &day)
        }
        Command::Upload { archive } => {
            let config = load_config(None);
            upload_archive(&config, &archive)
        }
        Command::Decrypt {
            archive,
            output,
            passphrase_file,
        } => decrypt(&archive, &output, passphrase_file.as_deref()),
    }
}

enum Command {
    Help,
    ListUploaded {
        artifacts_dir: Option<PathBuf>,
    },
    Seal {
        artifacts_dir: Option<PathBuf>,
        day: String,
    },
    Upload {
        archive: PathBuf,
    },
    Decrypt {
        archive: PathBuf,
        output: PathBuf,
        passphrase_file: Option<PathBuf>,
    },
}

fn parse_args(arguments: Vec<OsString>) -> Result<Command> {
    let mut arguments = arguments.into_iter();
    let mut artifacts_dir = None;
    let first = arguments.next().unwrap_or_default();
    let command = if first == "--help" || first == "-h" || first.is_empty() {
        ensure!(arguments.next().is_none(), "--help takes no arguments");
        return Ok(Command::Help);
    } else if first == "--artifacts-dir" {
        artifacts_dir = Some(PathBuf::from(
            arguments
                .next()
                .context("--artifacts-dir requires a path")?,
        ));
        arguments
            .next()
            .context("a command is required after --artifacts-dir")?
    } else {
        first
    };
    ensure!(command == "archive", "unknown command; run with --help");
    let action = arguments.next().context("archive action is required")?;
    match action.to_string_lossy().as_ref() {
        "list-uploaded" => {
            ensure!(
                arguments.next().is_none(),
                "archive list-uploaded takes no arguments"
            );
            Ok(Command::ListUploaded { artifacts_dir })
        }
        "seal" => {
            let day = arguments
                .next()
                .context("archive seal requires a YYYY-MM-DD day")?
                .into_string()
                .map_err(|_| anyhow!("archive day must be valid Unicode"))?;
            ensure!(
                arguments.next().is_none(),
                "archive seal takes exactly one day"
            );
            Ok(Command::Seal { artifacts_dir, day })
        }
        "upload" => {
            ensure!(
                artifacts_dir.is_none(),
                "--artifacts-dir does not apply to archive upload"
            );
            let archive = arguments
                .next()
                .map(PathBuf::from)
                .context("archive upload requires an archive path")?;
            ensure!(
                arguments.next().is_none(),
                "archive upload takes exactly one archive path"
            );
            Ok(Command::Upload { archive })
        }
        "decrypt" => {
            ensure!(
                artifacts_dir.is_none(),
                "--artifacts-dir does not apply to archive decrypt"
            );
            let archive = arguments
                .next()
                .map(PathBuf::from)
                .context("archive decrypt requires an archive path")?;
            let output = arguments
                .next()
                .map(PathBuf::from)
                .context("archive decrypt requires an output directory")?;
            let mut passphrase_file = None;
            while let Some(argument) = arguments.next() {
                ensure!(
                    argument == "--passphrase-file",
                    "unknown archive decrypt option"
                );
                ensure!(
                    passphrase_file.is_none(),
                    "--passphrase-file may only be provided once"
                );
                passphrase_file = Some(PathBuf::from(
                    arguments
                        .next()
                        .context("--passphrase-file requires a path")?,
                ));
            }
            ensure!(archive != output, "archive and output paths must differ");
            Ok(Command::Decrypt {
                archive,
                output,
                passphrase_file,
            })
        }
        _ => Err(anyhow!("unknown archive action; run with --help")),
    }
}

fn load_config(artifacts_dir: Option<PathBuf>) -> AppConfig {
    let mut config = AppConfig::load();
    if let Some(path) = artifacts_dir {
        config.activity_artifacts_dir = path;
    }
    config
}

fn list_uploaded(config: &AppConfig) -> Result<()> {
    let records = archive::uploaded_archives(config)?;
    if records.is_empty() {
        println!("No archives are currently stored by the receiver.");
        return Ok(());
    }
    println!("DAY         BYTES        LOCAL  SHA256");
    for record in records {
        println!(
            "{}  {:<12} {:<6} {}",
            record.day,
            record.size,
            if record.archive_present { "yes" } else { "no" },
            record.sha256
        );
    }
    Ok(())
}

fn upload_archive(config: &AppConfig, path: &Path) -> Result<()> {
    let uploaded = archive::upload_archive_manually(config, path)?;
    println!(
        "Uploaded archive: day={} path={} bytes={} sha256={}",
        uploaded.day,
        path.display(),
        uploaded.size,
        uploaded.sha256
    );
    Ok(())
}

fn seal_day(config: &AppConfig, day: &str) -> Result<()> {
    match archive::seal_day_manually(config, day)? {
        SealDayOutcome::Sealed { path, metadata } => println!(
            "Archived day={} path={} bytes={} sha256={}",
            metadata.day,
            path.display(),
            metadata.size,
            metadata.sha256
        ),
        SealDayOutcome::AlreadySealed { path, sha256, size } => println!(
            "Day already archived: day={} path={} bytes={} sha256={}",
            day,
            path.display(),
            size,
            sha256
        ),
    }
    Ok(())
}

fn decrypt(archive: &Path, output: &Path, passphrase_file: Option<&Path>) -> Result<()> {
    let passphrase = match passphrase_file {
        Some(path) => read_passphrase(path)?,
        None => Zeroizing::new(
            rpassword::prompt_password("Five-word archive passphrase: ")
                .context("failed to read archive passphrase")?,
        ),
    };
    ensure!(
        passphrase.split_whitespace().count() == 5,
        "passphrase must contain exactly five words"
    );
    let decrypted = decrypt_archive_file(archive, passphrase.trim())?;
    let day = extract_bundle(&decrypted.payload, output)?;
    println!("Restored archive day={} output={}", day, output.display());
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{Command, parse_args};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_archive_commands() {
        let Command::ListUploaded { artifacts_dir } = parse_args(args(&[
            "--artifacts-dir",
            "store",
            "archive",
            "list-uploaded",
        ]))
        .unwrap() else {
            panic!("expected list-uploaded")
        };
        assert_eq!(artifacts_dir, Some(PathBuf::from("store")));

        let Command::Seal { day, .. } =
            parse_args(args(&["archive", "seal", "2026-07-28"])).unwrap()
        else {
            panic!("expected seal")
        };
        assert_eq!(day, "2026-07-28");

        let Command::Upload { archive } =
            parse_args(args(&["archive", "upload", "archive.ashe"])).unwrap()
        else {
            panic!("expected upload")
        };
        assert_eq!(archive, PathBuf::from("archive.ashe"));

        let Command::Decrypt {
            archive,
            output,
            passphrase_file,
        } = parse_args(args(&[
            "archive",
            "decrypt",
            "archive.ashe",
            "restored",
            "--passphrase-file",
            "words.txt",
        ]))
        .unwrap()
        else {
            panic!("expected decrypt")
        };
        assert_eq!(archive, PathBuf::from("archive.ashe"));
        assert_eq!(output, PathBuf::from("restored"));
        assert_eq!(passphrase_file, Some(PathBuf::from("words.txt")));
    }

    #[test]
    fn rejects_ambiguous_archive_arguments() {
        assert!(parse_args(args(&["archive", "seal"])).is_err());
        assert!(parse_args(args(&["archive", "list-uploaded", "extra"])).is_err());
        assert!(parse_args(args(&["archive", "upload"])).is_err());
        assert!(
            parse_args(args(&[
                "--artifacts-dir",
                "store",
                "archive",
                "upload",
                "archive.ashe",
            ]))
            .is_err()
        );
        assert!(
            parse_args(args(&[
                "--artifacts-dir",
                "store",
                "archive",
                "decrypt",
                "archive.ashe",
                "restored",
            ]))
            .is_err()
        );
    }
}
