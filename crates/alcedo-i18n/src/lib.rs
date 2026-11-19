//! Translated UI text through the system gettext in glibc, linked dynamically.
//!
//! Each crate that shows text declares its gettext domain once at its root:
//!
//! ```ignore
//! const I18N_DOMAIN: &str = "alcedo";
//! ```
//!
//! and then uses the macros, which look that constant up in the calling crate.
//! `scripts/i18n.py extract` finds strings by the macro names `tr!`, `trs!`,
//! `trc!`, `trf!`, `ntr!` and `n_!`, so the first argument must be a plain
//! string literal. Write whole sentences with named `{placeholders}` and use `ntr!`
//! for anything counted; never join translated pieces, and never compare or
//! store translated text.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_ulong};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::Mutex;

/// Where installed catalogues live. Packaging sets `ALCEDO_INSTALL_LOCALEDIR`
/// at build time (the Flatpak uses `/app/share/locale`). It is a different name
/// from the run-time `ALCEDO_LOCALEDIR`, so a developer's override never ends
/// up compiled into a release build.
const DEFAULT_LOCALE_DIR: &str = match option_env!("ALCEDO_INSTALL_LOCALEDIR") {
    Some(dir) => dir,
    None => "/usr/share/locale",
};

/// glibc's value for `LC_ALL`.
const LC_ALL: c_int = 6;

/// gettext separates a message context from its msgid with EOT.
const CONTEXT_SEPARATOR: char = '\u{4}';

unsafe extern "C" {
    // All of these live in glibc, so no extra library is linked.
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
    fn bindtextdomain(domain: *const c_char, dir: *const c_char) -> *mut c_char;
    fn bind_textdomain_codeset(domain: *const c_char, codeset: *const c_char) -> *mut c_char;
    fn dgettext(domain: *const c_char, msgid: *const c_char) -> *mut c_char;
    fn dngettext(
        domain: *const c_char,
        msgid: *const c_char,
        plural: *const c_char,
        n: c_ulong,
    ) -> *mut c_char;
}

/// Why an app's text stays in English after [`init`].
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("the locale directory {0:?} is not usable text")]
    LocaleDirNotText(PathBuf),
    #[error("{0:?} is not a valid gettext domain")]
    BadDomain(String),
    #[error("gettext could not bind the {0} text domain")]
    BindFailed(String),
    #[error("the desktop locale is not installed (see `locale -a`)")]
    LocaleUnavailable,
}

/// The directory to read catalogues from: `ALCEDO_LOCALEDIR` at run time (for
/// development and tests), else the install location.
pub fn locale_dir() -> PathBuf {
    std::env::var_os("ALCEDO_LOCALEDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LOCALE_DIR))
}

/// Apply the user's locale and point gettext at `domain`'s catalogues.
///
/// Call at the very start of the program, before any thread is spawned:
/// `setlocale` is not thread-safe, and threads may format text before GTK
/// would set the locale itself. The language comes from `LANGUAGE`, `LC_ALL`,
/// `LC_MESSAGES` and `LANG`. Calling it again for another domain is fine.
/// Logging may not be set up yet, so the caller reports the error.
pub fn init(domain: &str) -> Result<(), InitError> {
    // SAFETY: the argument is a valid NUL-terminated string. setlocale is not
    // thread-safe; callers run this before other threads exist (see above).
    let locale_applied = unsafe { !setlocale(LC_ALL, c"".as_ptr()).is_null() };

    let dir = locale_dir();
    let dir_c = dir
        .to_str()
        .and_then(|text| CString::new(text).ok())
        .ok_or_else(|| InitError::LocaleDirNotText(dir.clone()))?;
    let domain_c = CString::new(domain).map_err(|_| InitError::BadDomain(domain.to_string()))?;
    // SAFETY: all pointers are valid NUL-terminated strings for the duration of
    // the calls, gettext copies what it keeps, and only the returned pointers'
    // nullness is read.
    let bound = unsafe {
        let dir_result = bindtextdomain(domain_c.as_ptr(), dir_c.as_ptr());
        let codeset_result = bind_textdomain_codeset(domain_c.as_ptr(), c"UTF-8".as_ptr());
        !dir_result.is_null() && !codeset_result.is_null()
    };
    if !bound {
        return Err(InitError::BindFailed(domain.to_string()));
    }
    if !locale_applied {
        return Err(InitError::LocaleUnavailable);
    }
    Ok(())
}

/// Translate `msgid` in `domain`; returns it unchanged when there is no
/// translation.
pub fn translate(domain: &str, msgid: &str) -> String {
    let (Ok(domain_c), Ok(msgid_c)) = (CString::new(domain), CString::new(msgid)) else {
        return msgid.to_string();
    };
    // SAFETY: both arguments are valid NUL-terminated strings. The result is
    // either `msgid_c` (alive here) or a string in the loaded catalogue, which
    // gettext keeps for the life of the process; it is copied at once.
    unsafe { owned(dgettext(domain_c.as_ptr(), msgid_c.as_ptr())) }
        .unwrap_or_else(|| msgid.to_string())
}

/// Translate `msgid` within a context that tells translators how it is used
/// (for example a button versus a heading).
pub fn translate_in_context(domain: &str, context: &str, msgid: &str) -> String {
    let key = format!("{context}{CONTEXT_SEPARATOR}{msgid}");
    let translated = translate(domain, &key);
    if translated == key {
        msgid.to_string()
    } else {
        translated
    }
}

/// Pick the singular or plural form for `count` in the user's language.
pub fn translate_plural(domain: &str, singular: &str, plural: &str, count: u64) -> String {
    let english = if count == 1 { singular } else { plural };
    let (Ok(domain_c), Ok(singular_c), Ok(plural_c)) = (
        CString::new(domain),
        CString::new(singular),
        CString::new(plural),
    ) else {
        return english.to_string();
    };
    let n = c_ulong::try_from(count).unwrap_or(c_ulong::MAX);
    // SAFETY: as in `translate`; the result is one of the two arguments or a
    // catalogue string, copied at once.
    unsafe {
        owned(dngettext(
            domain_c.as_ptr(),
            singular_c.as_ptr(),
            plural_c.as_ptr(),
            n,
        ))
    }
    .unwrap_or_else(|| english.to_string())
}

/// Copy a string gettext returned.
///
/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated string.
unsafe fn owned(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null and NUL-terminated per this function's contract.
    let text = unsafe { CStr::from_ptr(ptr) };
    text.to_str().ok().map(str::to_string)
}

/// Translations already leaked by [`translate_static`], by domain and English
/// text. Guarded by a mutex; the critical section only does a map lookup or
/// insert (the gettext lookup for a new entry runs inside it, once per text).
type StaticCache = HashMap<(&'static str, &'static str), &'static str>;
static STATIC_TRANSLATIONS: Mutex<Option<StaticCache>> = Mutex::new(None);

/// Like [`translate`], for APIs that take `&'static str` (menu labels,
/// empty-state text, help navigation). Each distinct text is leaked once and
/// then reused, so memory is bounded by the number of such literals, not by
/// how often the UI is rebuilt. The language cannot change while the app runs.
pub fn translate_static(domain: &'static str, msgid: &'static str) -> &'static str {
    let mut guard = STATIC_TRANSLATIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    cache
        .entry((domain, msgid))
        .or_insert_with(|| Box::leak(translate(domain, msgid).into_boxed_str()))
}

/// Replace each `{name}` in `template` with the matching value. Unknown names
/// are left as written. One pass over the template, so a value that itself
/// contains `{name}` is never substituted again.
pub fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match args.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => out.push_str(&value.to_string()),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// `tr!("Open")` is the translation of a fixed string in the calling crate's
/// `I18N_DOMAIN`.
// `crate::I18N_DOMAIN` deliberately names the calling crate's domain.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! tr {
    ($msgid:literal) => {
        $crate::translate(crate::I18N_DOMAIN, $msgid)
    };
}

/// `trs!("Open")` is [`tr!`] for APIs that need a `&'static str`.
// `crate::I18N_DOMAIN` deliberately names the calling crate's domain.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! trs {
    ($msgid:literal) => {
        $crate::translate_static(crate::I18N_DOMAIN, $msgid)
    };
}

/// `n_!("Unread")` marks text for translation without translating it, for
/// `const` and `static` tables built before the locale is known. Translate it
/// where it is shown with [`trm!`]; keep using the untranslated value as a key.
#[macro_export]
macro_rules! n_ {
    ($msgid:literal) => {
        $msgid
    };
}

/// `trm!(text)` translates a `&'static str` that was marked with [`n_!`].
// `crate::I18N_DOMAIN` deliberately names the calling crate's domain.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! trm {
    ($marked:expr) => {
        $crate::translate_static(crate::I18N_DOMAIN, $marked)
    };
}

/// `trc!("button", "Open")` translates with a context for translators.
// `crate::I18N_DOMAIN` deliberately names the calling crate's domain.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! trc {
    ($context:literal, $msgid:literal) => {
        $crate::translate_in_context(crate::I18N_DOMAIN, $context, $msgid)
    };
}

/// `trf!("Saved {name}.", name = value)` translates a sentence and then fills
/// its named placeholders, so word order can differ by language.
// `crate::I18N_DOMAIN` deliberately names the calling crate's domain.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! trf {
    ($msgid:literal $(, $name:ident = $value:expr)+ $(,)?) => {
        $crate::fill(
            &$crate::translate(crate::I18N_DOMAIN, $msgid),
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// `ntr!("{n} message", "{n} messages", count)` picks the plural form for
/// `count` and fills `{n}` with it. Extra named values may follow `count`.
// `crate::I18N_DOMAIN` deliberately names the calling crate's domain.
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! ntr {
    ($singular:literal, $plural:literal, $count:expr $(, $name:ident = $value:expr)* $(,)?) => {{
        let count: u64 = $count as u64;
        $crate::fill(
            &$crate::translate_plural(crate::I18N_DOMAIN, $singular, $plural, count),
            &[("n", &count as &dyn ::std::fmt::Display)
              $(, (stringify!($name), &$value as &dyn ::std::fmt::Display))*],
        )
    }};
}

/// The macros read the domain from the calling crate's root, as in any user crate.
#[cfg(test)]
const I18N_DOMAIN: &str = "alcedo-i18n-tests";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_named_placeholders_in_any_order() {
        let text = fill("{b} then {a}", &[("a", &1), ("b", &"two")]);
        assert_eq!(text, "two then 1");
    }

    #[test]
    fn leaves_unknown_and_unclosed_placeholders_as_written() {
        assert_eq!(fill("x {nope} y", &[("a", &1)]), "x {nope} y");
        assert_eq!(fill("x {open", &[("open", &1)]), "x {open");
    }

    #[test]
    fn does_not_substitute_inside_substituted_values() {
        let text = fill("{a} {b}", &[("a", &"{b}"), ("b", &"B")]);
        assert_eq!(text, "{b} B");
    }

    #[test]
    fn static_translation_is_cached_by_text_not_by_pointer() {
        // Two distinct allocations with the same text: identical literals could
        // share one address, which would hide a missing cache.
        let first_key: &'static str = Box::leak(String::from("Same static text").into_boxed_str());
        let second_key: &'static str = Box::leak(String::from("Same static text").into_boxed_str());
        let first = translate_static(crate::I18N_DOMAIN, first_key);
        let second = translate_static(crate::I18N_DOMAIN, second_key);
        assert_eq!(first, "Same static text");
        assert!(std::ptr::eq(first, second));
        assert!(!std::ptr::eq(second, second_key));
    }

    #[test]
    fn without_a_catalogue_text_comes_back_in_english() {
        assert_eq!(crate::tr!("No such key anywhere"), "No such key anywhere");
        assert_eq!(crate::trc!("button", "Open"), "Open");
        assert_eq!(crate::trf!("Hello {name}", name = "Ana"), "Hello Ana");
        assert_eq!(crate::ntr!("{n} item", "{n} items", 1), "1 item");
        assert_eq!(crate::ntr!("{n} item", "{n} items", 3), "3 items");
        assert_eq!(crate::ntr!("{n} item", "{n} items", 0), "0 items");
    }

    #[test]
    fn marked_text_is_kept_as_written_and_translated_on_use() {
        const MARKED: &str = crate::n_!("Marked text");
        assert_eq!(MARKED, "Marked text");
        assert_eq!(crate::trm!(MARKED), "Marked text");
    }

    #[test]
    fn text_with_a_nul_byte_is_returned_unchanged() {
        assert_eq!(translate(crate::I18N_DOMAIN, "a\0b"), "a\0b");
    }
}
