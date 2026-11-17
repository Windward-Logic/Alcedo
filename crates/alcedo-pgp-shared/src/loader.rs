//! Finds, loads and validates the OpenPGP shared library, once per process.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alcedo_pgp_api::wire::{
    ABI_VERSION, AbiVersionFn, BuildIdFn, CallFn, FfiBuffer, FreeFn, INSTALLED_LIBRARY_FILE_NAME,
    LIBRARY_FILE_NAME, LIBRARY_PATH_ENV, SYM_ABI_VERSION, SYM_BUILD_ID, SYM_CALL, SYM_FREE,
};
use libloading::Library;

/// The process-wide load result. Initialised once and never dropped (statics
/// are not destroyed), so a loaded library stays mapped until the process
/// exits.
static LOADED: OnceLock<Result<Loaded, String>> = OnceLock::new();

/// A loaded library and the four entry points copied out of it.
///
/// Copying the function pointers out of `libloading::Symbol` drops the borrow
/// that ties them to `Library`. That is sound here because `load` moves each
/// `Loaded` it builds straight into [`LOADED`], which never drops it, so
/// `_library` is never unloaded and the pointers stay valid for the life of
/// the process (moving a `Library` handle does not unload it). The fields are
/// private so no other code can build or drop one.
pub(crate) struct Loaded {
    _library: Library,
    path: PathBuf,
    abi_version_fn: AbiVersionFn,
    build_id_fn: BuildIdFn,
    call_fn: CallFn,
    free_fn: FreeFn,
}

/// The library, loading it on first use. The error is the reason it could not
/// be loaded; it is computed once and returned on every later call.
pub(crate) fn loaded() -> Result<&'static Loaded, &'static str> {
    LOADED
        .get_or_init(load_and_log)
        .as_ref()
        .map_err(String::as_str)
}

impl Loaded {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn abi_version(&self) -> u32 {
        // SAFETY: the pointer was resolved from `SYM_ABI_VERSION`, whose ABI
        // type is `AbiVersionFn`, and the library is never unloaded (see
        // `Loaded`).
        unsafe { (self.abi_version_fn)() }
    }

    pub(crate) fn build_id(&self) -> String {
        // SAFETY: as in `abi_version`, for `SYM_BUILD_ID` / `BuildIdFn`.
        let ptr = unsafe { (self.build_id_fn)() };
        if ptr.is_null() {
            return String::new();
        }
        // SAFETY: the ABI promises a non-null result is a static,
        // NUL-terminated string that is never freed.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    /// Sends one request; the returned guard frees the reply buffer on drop.
    pub(crate) fn call(&self, request: &[u8]) -> (i32, LibraryBuffer) {
        let mut out = FfiBuffer::EMPTY;
        // SAFETY: the pointer was resolved from `SYM_CALL`, whose ABI type is
        // `CallFn`, and the library is never unloaded. `request` points to
        // `request.len()` live bytes for the whole call, and `out` is a valid,
        // writable `FfiBuffer`.
        let code = unsafe { (self.call_fn)(request.as_ptr(), request.len(), &mut out) };
        let buffer = LibraryBuffer {
            buffer: out,
            free_fn: self.free_fn,
        };
        (code, buffer)
    }
}

/// A reply buffer owned by the library. Dropping it hands the buffer back to
/// the library's free function, so it is released on every path, including
/// errors.
pub(crate) struct LibraryBuffer {
    buffer: FfiBuffer,
    free_fn: FreeFn,
}

impl LibraryBuffer {
    pub(crate) fn bytes(&self) -> &[u8] {
        if self.buffer.ptr.is_null() {
            return &[];
        }
        // SAFETY: a non-null buffer written by the library's call function
        // holds `len` initialised bytes until it is freed, which only happens
        // in `drop`, after every borrow of `self` has ended.
        unsafe { std::slice::from_raw_parts(self.buffer.ptr, self.buffer.len) }
    }
}

impl Drop for LibraryBuffer {
    fn drop(&mut self) {
        if self.buffer.ptr.is_null() {
            return;
        }
        let buffer = std::mem::replace(&mut self.buffer, FfiBuffer::EMPTY);
        // SAFETY: `free_fn` was resolved from `SYM_FREE` (ABI type `FreeFn`)
        // in a library that is never unloaded, and `buffer` is the unmodified
        // value its call function wrote, freed exactly once because `self`
        // now holds EMPTY.
        unsafe { (self.free_fn)(buffer) };
    }
}

fn load_and_log() -> Result<Loaded, String> {
    let result = load();
    match &result {
        Ok(loaded) => tracing::info!(
            path = %loaded.path.display(),
            abi = loaded.abi_version(),
            build_id = %loaded.build_id(),
            "loaded OpenPGP library"
        ),
        Err(reason) => tracing::warn!(%reason, "OpenPGP library unavailable"),
    }
    result
}

fn load() -> Result<Loaded, String> {
    load_from(find_library()?)
}

fn load_from(path: PathBuf) -> Result<Loaded, String> {
    // SAFETY: loading runs the library's initialisers. The file is the one the
    // user named in `ALCEDO_PGP_LIB` or one installed next to the app; running
    // its code is the point of a replaceable library, and no Rust invariant
    // depends on what the initialisers do.
    let library = unsafe { Library::new(&path) }
        .map_err(|err| format!("cannot load {}: {err}", path.display()))?;

    // Check the ABI before resolving anything else: a library built for
    // another ABI may name or type its other symbols differently.
    // SAFETY: `SYM_ABI_VERSION` has type `AbiVersionFn` in every ABI version.
    let abi_version_fn: AbiVersionFn = unsafe { resolve(&library, SYM_ABI_VERSION) }?;
    // SAFETY: as above; the library is still loaded.
    let found = unsafe { abi_version_fn() };
    check_abi(found)?;

    // SAFETY: the library reports `ABI_VERSION`, whose symbol types are the
    // ones named here.
    let build_id_fn: BuildIdFn = unsafe { resolve(&library, SYM_BUILD_ID) }?;
    // SAFETY: as above.
    let call_fn: CallFn = unsafe { resolve(&library, SYM_CALL) }?;
    // SAFETY: as above.
    let free_fn: FreeFn = unsafe { resolve(&library, SYM_FREE) }?;

    Ok(Loaded {
        _library: library,
        path,
        abi_version_fn,
        build_id_fn,
        call_fn,
        free_fn,
    })
}

fn check_abi(found: u32) -> Result<(), String> {
    if found == ABI_VERSION {
        return Ok(());
    }
    Err(format!(
        "OpenPGP library ABI {found}, expected {ABI_VERSION}"
    ))
}

/// Copies a function pointer out of `library`.
///
/// # Safety
///
/// `T` must be the exact function pointer type of the symbol `name`, and the
/// pointer must not be called after `library` is unloaded.
unsafe fn resolve<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    // SAFETY: the caller guarantees `T` matches the symbol's type.
    let symbol = unsafe { library.get::<T>(name) }.map_err(|err| {
        let printable = String::from_utf8_lossy(name.strip_suffix(b"\0").unwrap_or(name));
        format!("OpenPGP library lacks symbol {printable}: {err}")
    })?;
    Ok(*symbol)
}

/// The library path: `ALCEDO_PGP_LIB` alone if set, otherwise the first file
/// that exists among the locations a package or `cargo` puts it.
fn find_library() -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os(LIBRARY_PATH_ENV) {
        return explicit_library(PathBuf::from(explicit));
    }
    let exe = std::env::current_exe()
        .map_err(|err| format!("cannot locate the running executable: {err}"))?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| format!("executable path {} has no parent", exe.display()))?;
    let candidates = bundled_candidates(exe_dir);
    if let Some(found) = candidates.iter().find(|path| path.is_file()) {
        return Ok(found.clone());
    }
    let searched: Vec<String> = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    Err(format!(
        "{LIBRARY_FILE_NAME} not found (searched {})",
        searched.join(", ")
    ))
}

/// Resolves `ALCEDO_PGP_LIB` to an absolute path. `dlopen` searches the system
/// library path for a name without a slash, so a relative value would load a
/// different file from the one checked here.
fn explicit_library(path: PathBuf) -> Result<PathBuf, String> {
    match std::fs::canonicalize(&path) {
        Ok(resolved) if resolved.is_file() => Ok(resolved),
        _ => Err(format!(
            "{LIBRARY_PATH_ENV} is set to {}, which is not an existing file",
            path.display()
        )),
    }
}

/// Search order after `ALCEDO_PGP_LIB`: the installed package layout, then
/// next to the executable, then (only for cargo test binaries) one level up.
fn bundled_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![
        exe_dir
            .join("../lib/alcedo")
            .join(INSTALLED_LIBRARY_FILE_NAME),
        exe_dir.join(LIBRARY_FILE_NAME),
    ];
    // cargo test binaries live in target/<profile>/deps/.
    if exe_dir.file_name() == Some(std::ffi::OsStr::new("deps")) {
        candidates.push(exe_dir.join("..").join(LIBRARY_FILE_NAME));
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_candidates_follow_the_documented_order() {
        assert_eq!(
            bundled_candidates(Path::new("/opt/alcedo/bin")),
            [
                PathBuf::from("/opt/alcedo/bin/../lib/alcedo/libalcedo_pgp_ffi.so.1"),
                PathBuf::from("/opt/alcedo/bin/libalcedo_pgp_ffi.so"),
            ]
        );
        assert_eq!(
            bundled_candidates(Path::new("/x/target/debug/deps")),
            [
                PathBuf::from("/x/target/debug/deps/../lib/alcedo/libalcedo_pgp_ffi.so.1"),
                PathBuf::from("/x/target/debug/deps/libalcedo_pgp_ffi.so"),
                PathBuf::from("/x/target/debug/deps/../libalcedo_pgp_ffi.so"),
            ]
        );
    }

    #[test]
    fn installed_name_carries_the_abi_version() {
        assert_eq!(
            INSTALLED_LIBRARY_FILE_NAME,
            format!("{LIBRARY_FILE_NAME}.{ABI_VERSION}")
        );
    }

    #[test]
    fn explicit_relative_path_resolves_to_an_absolute_file() {
        let resolved = explicit_library(PathBuf::from("Cargo.toml")).expect("exists");
        assert!(resolved.is_absolute(), "{}", resolved.display());
        assert!(resolved.ends_with("alcedo-pgp-shared/Cargo.toml"));
    }

    #[test]
    fn explicit_path_that_is_missing_or_a_directory_is_an_error() {
        for bad in ["does-not-exist.so", "src"] {
            let err = explicit_library(PathBuf::from(bad)).expect_err(bad);
            assert!(err.contains(bad), "{err}");
        }
    }

    #[test]
    fn abi_mismatch_is_rejected() {
        assert_eq!(check_abi(ABI_VERSION), Ok(()));
        assert_eq!(
            check_abi(ABI_VERSION + 1),
            Err(format!(
                "OpenPGP library ABI {}, expected {ABI_VERSION}",
                ABI_VERSION + 1
            ))
        );
    }

    #[test]
    fn rejects_a_library_without_the_abi_symbol() {
        // Rust's std already loads libgcc_s; it exports no alcedo symbols.
        let err = load_from(PathBuf::from("libgcc_s.so.1"))
            .err()
            .expect("rejected");
        assert!(
            err.contains("lacks symbol alcedo_pgp_ffi_abi_version"),
            "{err}"
        );
    }

    #[test]
    fn rejects_a_file_that_is_not_a_library() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let err = load_from(path).err().expect("rejected");
        assert!(err.starts_with("cannot load "), "{err}");
    }
}
