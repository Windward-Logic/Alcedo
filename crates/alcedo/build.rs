use std::process::Command;

fn main() {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let schema_dir = out_dir.join("schemas");
    std::fs::create_dir_all(&schema_dir).expect("create schema dir");

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/dev.alcedo.Mail.gschema.xml");
    std::fs::copy(&src, schema_dir.join("dev.alcedo.Mail.gschema.xml")).expect("copy gschema");

    let status = Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status()
        .expect("run glib-compile-schemas");
    assert!(
        status.success(),
        "glib-compile-schemas failed with status {status}"
    );

    println!("cargo:rustc-env=ALCEDO_SCHEMA_DIR={}", schema_dir.display());
    println!("cargo:rerun-if-changed={}", src.display());
}
