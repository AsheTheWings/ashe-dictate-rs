fn main() {
    println!("cargo:rerun-if-env-changed=ASHE_BUILD_ID");
    println!("cargo:rerun-if-changed=data/ashe-dictate-rs.ico");
    let build_id = std::env::var("ASHE_BUILD_ID").unwrap_or_else(|_| "dev".to_string());
    println!("cargo:rustc-env=ASHE_BUILD_ID={build_id}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon();
    }
}

fn embed_windows_icon() {
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let icon_path = manifest_dir.join("data").join("ashe-dictate-rs.ico");
    let rc_path = out_dir.join("ashe-dictate-rs.rc");
    let res_path = out_dir.join("ashe-dictate-rs.res");

    let mut rc_file = std::fs::File::create(&rc_path).expect("create Windows resource script");
    writeln!(
        rc_file,
        "1 ICON \"{}\"",
        icon_path.display().to_string().replace('\\', "\\\\")
    )
    .expect("write Windows icon resource");

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
            "cargo:rustc-link-arg-bin=ashe-dictate-rs={}",
            res_path.display()
        );
    }
}
