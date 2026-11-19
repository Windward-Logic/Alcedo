//! Compiles the real catalogues with `scripts/i18n.py` and reads them back
//! through glibc gettext, starting from `alcedo_i18n::init` so the locale it
//! applies is part of the test. This is the only test in the binary, so no
//! other thread touches the locale or the environment while it runs.

use std::ffi::{CStr, c_char, c_int};
use std::path::Path;
use std::process::Command;

/// The macros read the domain from the calling crate's root.
const I18N_DOMAIN: &str = "alcedo";

unsafe extern "C" {
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

const LC_ALL: c_int = 6;

/// An installed locale under which gettext honours `LANGUAGE`. glibc ignores
/// `LANGUAGE` in "C" and "C.UTF-8", so a named locale is needed.
fn installed_named_locale() -> &'static CStr {
    [c"en_US.UTF-8", c"en_US.utf8", c"en_GB.UTF-8", c"en_GB.utf8"]
        .into_iter()
        .find(|name| {
            // SAFETY: valid NUL-terminated strings; single test per binary.
            unsafe {
                let found = !setlocale(LC_ALL, name.as_ptr()).is_null();
                setlocale(LC_ALL, c"C".as_ptr());
                found
            }
        })
        .expect(
            "this test needs an English UTF-8 locale (en_US.UTF-8 or en_GB.UTF-8); \
             generate one with locale-gen",
        )
}

#[test]
fn shipped_catalogues_translate_through_system_gettext() {
    let locale = installed_named_locale();
    let out = tempfile::tempdir().expect("temp dir");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new("python3")
        .args(["-I", "scripts/i18n.py", "compile"])
        .arg(out.path())
        .current_dir(&root)
        .status()
        .expect("python3 runs");
    assert!(status.success(), "catalogue compile failed");

    // SAFETY: the only test in this binary, so no other thread reads the
    // environment while it is changed.
    unsafe {
        std::env::set_var("LC_ALL", locale.to_str().expect("ASCII name"));
        std::env::set_var("ALCEDO_LOCALEDIR", out.path());
        std::env::set_var("LANGUAGE", "fr");
    }
    // The process is still in the "C" locale here; `init` must apply LC_ALL.
    alcedo_i18n::init(I18N_DOMAIN).expect("translations initialise");

    assert_eq!(alcedo_i18n::tr!("Inbox"), "Boîte de réception");
    assert_eq!(
        alcedo_i18n::trf!(
            "Signed by {email}. The signature is valid.",
            email = "ana@example.org"
        ),
        "Signé par ana@example.org. La signature est valide."
    );
    assert_eq!(
        alcedo_i18n::ntr!("{n} message", "{n} messages", 3),
        "3 messages"
    );
    assert_eq!(
        alcedo_i18n::ntr!("Message archived", "{n} messages archived", 2),
        "2 messages archivés"
    );
    assert_eq!(
        alcedo_i18n::trc!("strftime pattern: short month, day", "%b %-d"),
        "%-d %b"
    );
}
