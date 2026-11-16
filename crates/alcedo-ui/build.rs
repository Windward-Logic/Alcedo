use std::process::Command;

fn compile_gresource(sourcedir: &std::path::Path, xml: &std::path::Path, target: &std::path::Path) {
    let status = Command::new("glib-compile-resources")
        .arg(format!("--sourcedir={}", sourcedir.display()))
        .arg(format!("--target={}", target.display()))
        .arg(xml)
        .status()
        .expect("run glib-compile-resources");
    assert!(
        status.success(),
        "glib-compile-resources failed for {} with status {status}",
        xml.display()
    );
}

fn main() {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest_dir =
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    let icons_dir = manifest_dir.join("../../data/icons");
    let icons_xml = icons_dir.join("icons.gresource.xml");
    let icons_target = out_dir.join("alcedo.gresource");
    compile_gresource(&icons_dir, &icons_xml, &icons_target);
    println!("cargo:rerun-if-changed={}", icons_xml.display());
    for rel in [
        "hicolor/scalable/apps/dev.alcedo.Mail.svg",
        "hicolor/scalable/actions/alcedo-rail-symbolic.svg",
        "hicolor/scalable/actions/alcedo-rail-hide-symbolic.svg",
        "hicolor/scalable/actions/alcedo-folder-pane-symbolic.svg",
        "hicolor/scalable/actions/alcedo-compose-symbolic.svg",
        "hicolor/scalable/actions/alcedo-mail-preview-symbolic.svg",
        "hicolor/scalable/actions/alcedo-columns-symbolic.svg",
        "hicolor/scalable/actions/alcedo-reading-layout-symbolic.svg",
        "hicolor/scalable/actions/alcedo-send-symbolic.svg",
        "hicolor/scalable/actions/alcedo-rules-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-archive-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-attachment-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-drafts-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-everything-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-folder-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-followup-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-inbox-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-junk-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-outbox-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-people-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-saved-search-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-search-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-sent-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-snooze-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-star-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-trash-symbolic.svg",
        "hicolor/scalable/actions/alcedo-sidebar-unread-symbolic.svg",
        "hicolor/48x48/actions/alcedo-mail.png",
        "hicolor/48x48/actions/alcedo-contacts.png",
        "hicolor/48x48/actions/alcedo-compose.png",
        "hicolor/256x256/actions/alcedo-mail.png",
        "hicolor/256x256/actions/alcedo-contacts.png",
        "hicolor/256x256/actions/alcedo-compose.png",
    ] {
        println!("cargo:rerun-if-changed={}", icons_dir.join(rel).display());
    }

    let resources_dir = manifest_dir.join("../../data/resources");
    let resources_xml = resources_dir.join("resources.gresource.xml");
    let resources_target = out_dir.join("alcedo-resources.gresource");
    compile_gresource(&resources_dir, &resources_xml, &resources_target);
    println!("cargo:rerun-if-changed={}", resources_xml.display());
    println!(
        "cargo:rerun-if-changed={}",
        resources_dir.join("compose/editor.html").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        resources_dir.join("style.css").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        resources_dir.join("help/index.html").display()
    );

    // GTK tests construct widgets that read GSettings. Compile the schema into
    // OUT_DIR so those tests do not abort when it is not installed on the system
    // (the binary crate does the same for the running app).
    let schema_src = manifest_dir.join("../../data/dev.alcedo.Mail.gschema.xml");
    let schema_dir = out_dir.join("schemas");
    std::fs::create_dir_all(&schema_dir).expect("create schema dir");
    std::fs::copy(&schema_src, schema_dir.join("dev.alcedo.Mail.gschema.xml"))
        .expect("copy gschema");
    let status = Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status()
        .expect("run glib-compile-schemas");
    assert!(
        status.success(),
        "glib-compile-schemas failed with status {status}"
    );
    println!("cargo:rustc-env=ALCEDO_SCHEMA_DIR={}", schema_dir.display());
    println!("cargo:rerun-if-changed={}", schema_src.display());
}
