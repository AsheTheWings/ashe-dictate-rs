fn main() {
    println!("cargo:rerun-if-env-changed=ASHE_BUILD_ID");
    println!("cargo:rerun-if-env-changed=ASHE_ARCHIVE_RECIPIENT_FILE");
    println!("cargo:rerun-if-env-changed=ASHE_ARCHIVE_RECIPIENT_JSON");
    println!("cargo:rerun-if-changed=assets/ashe-worker.ico");
    let build_id = std::env::var("ASHE_BUILD_ID").unwrap_or_else(|_| "dev".to_string());
    println!("cargo:rustc-env=ASHE_BUILD_ID={build_id}");
    embed_archive_recipient();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon();
    }
}

fn embed_archive_recipient() {
    let raw = std::env::var("ASHE_ARCHIVE_RECIPIENT_FILE")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map(|path| {
            println!("cargo:rerun-if-changed={path}");
            std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("failed to read archive recipient file {path}: {error}")
            })
        })
        .or_else(|| std::env::var("ASHE_ARCHIVE_RECIPIENT_JSON").ok())
        .unwrap_or_default();
    if raw.trim().is_empty() {
        println!("cargo:rustc-env=ASHE_ARCHIVE_RECIPIENT_JSON=");
        return;
    }
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("archive recipient must be valid JSON");
    assert!(value.is_object(), "archive recipient must be a JSON object");
    let compact = serde_json::to_string(&value).expect("serialize archive recipient");
    println!("cargo:rustc-env=ASHE_ARCHIVE_RECIPIENT_JSON={compact}");
}

fn embed_windows_icon() {
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let icon_path = manifest_dir.join("assets").join("ashe-worker.ico");
    let rc_path = out_dir.join("ashe-worker.rc");
    let res_path = out_dir.join("ashe-worker.res");

    let mut rc_file = std::fs::File::create(&rc_path).expect("create Windows resource script");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());
    let mut parts = version
        .split('.')
        .take(3)
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .collect::<Vec<_>>();
    parts.resize(3, 0);
    writeln!(
        rc_file,
        r#"1 ICON "{}"
1 VERSIONINFO
FILEVERSION {},{},{},0
PRODUCTVERSION {},{},{},0
FILETYPE 1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Ashe Services"
      VALUE "FileDescription", "Ashe Worker"
      VALUE "FileVersion", "{}"
      VALUE "InternalName", "ashe-worker"
      VALUE "OriginalFilename", "ashe-worker.exe"
      VALUE "ProductName", "Ashe Worker"
      VALUE "ProductVersion", "{}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END"#,
        icon_path.display().to_string().replace('\\', "\\\\"),
        parts[0],
        parts[1],
        parts[2],
        parts[0],
        parts[1],
        parts[2],
        version,
        version,
    )
    .expect("write Windows resources");

    let windres = std::env::var("WINDRES").unwrap_or_else(|_| {
        if std::env::var("TARGET").is_ok_and(|target| target.contains("windows-gnu")) {
            "x86_64-w64-mingw32-windres".to_string()
        } else {
            "windres".to_string()
        }
    });
    let status = Command::new(&windres)
        .arg(&rc_path)
        .arg("-O")
        .arg("coff")
        .arg("-o")
        .arg(&res_path)
        .status()
        .expect("run windres to compile Windows icon resource");
    assert!(status.success(), "windres failed while embedding app icon");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        println!(
            "cargo:rustc-link-arg-bin=ashe-worker={}",
            res_path.display()
        );
    }
}
