//! Bakes a build identifier into the library so the app can report which
//! OpenPGP library it loaded (for example a user's own relinked build).

const BUILD_ID_ENV: &str = "ALCEDO_PGP_BUILD_ID";
const DEFAULT_BUILD_ID: &str = "unmodified";
const MAX_BUILD_ID_LEN: usize = 64;

fn is_valid_build_id(id: &str) -> bool {
    (1..=MAX_BUILD_ID_LEN).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_graphic())
}

fn main() {
    println!("cargo:rerun-if-env-changed={BUILD_ID_ENV}");
    let id = match std::env::var(BUILD_ID_ENV) {
        Ok(id) => id,
        Err(std::env::VarError::NotPresent) => DEFAULT_BUILD_ID.to_owned(),
        Err(std::env::VarError::NotUnicode(raw)) => {
            panic!("{BUILD_ID_ENV} must be printable ASCII, got {raw:?}")
        }
    };
    assert!(
        is_valid_build_id(&id),
        "{BUILD_ID_ENV} must be 1-{MAX_BUILD_ID_LEN} printable ASCII characters without spaces, got {id:?}"
    );
    println!("cargo:rustc-env=ALCEDO_PGP_FFI_BUILD_ID={id}");
}
