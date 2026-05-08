fn main() {
    println!("cargo:rerun-if-env-changed=ASHE_BUILD_ID");
    let build_id = std::env::var("ASHE_BUILD_ID").unwrap_or_else(|_| "dev".to_string());
    println!("cargo:rustc-env=ASHE_BUILD_ID={build_id}");
}
