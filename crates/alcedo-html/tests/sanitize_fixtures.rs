use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use alcedo_html::{
    SanitizeOptions, html_has_remote_images, rewrite_cids, sanitize_html, strip_trackers,
};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read fixture {name}: {err}"))
}

#[test]
fn script_tag_removed() {
    let html = fixture("script_tag.html");
    let sanitized = sanitize_html(&html, SanitizeOptions::default());

    assert!(!sanitized.to_ascii_lowercase().contains("<script"));
    assert!(!sanitized.contains("alert"));
    assert!(sanitized.contains("Hello"));
    assert!(sanitized.contains("World"));
}

#[test]
fn one_by_one_img_removed() {
    let html = fixture("tracker_1x1.html");
    let stripped = strip_trackers(&html);

    assert!(!stripped.contains("pixel.example.com"));
    assert!(stripped.contains("Newsletter"));
    assert!(stripped.contains("Content"));

    let sanitized = sanitize_html(&html, SanitizeOptions::default());
    assert!(!sanitized.contains("pixel.example.com"));
    assert!(!html_has_remote_images(&html));
}

#[test]
fn remote_img_removed_unless_allowed() {
    let html = fixture("remote_img.html");

    let blocked = sanitize_html(
        &html,
        SanitizeOptions {
            allow_remote_images: false,
        },
    );
    assert!(!blocked.contains("images.example.com"));
    assert!(blocked.contains("Photo"));

    let allowed = sanitize_html(
        &html,
        SanitizeOptions {
            allow_remote_images: true,
        },
    );
    assert!(allowed.contains("images.example.com"));
    assert!(html_has_remote_images(&html));
}

#[test]
fn cid_preserved() {
    let html = fixture("cid_img.html");

    let sanitized = sanitize_html(
        &html,
        SanitizeOptions {
            allow_remote_images: false,
        },
    );
    assert!(sanitized.contains("cid:logo@example.com"));
    assert!(!sanitized.contains("images.example.com"));
    assert!(!html_has_remote_images(&html));

    let mut map = HashMap::new();
    map.insert("logo@example.com".to_string(), "deadbeef".to_string());
    let rewritten = rewrite_cids(&sanitized, &map);
    assert!(rewritten.contains("alcedo-cid://deadbeef"));

    let resanitized = sanitize_html(
        &rewritten,
        SanitizeOptions {
            allow_remote_images: false,
        },
    );
    assert!(
        resanitized.contains("alcedo-cid://deadbeef"),
        "reader re-sanitize must keep rewritten CID images"
    );
}
