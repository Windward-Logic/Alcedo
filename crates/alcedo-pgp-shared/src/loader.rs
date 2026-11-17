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
    let path = find_library()?;
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
    if found != ABI_VERSION {
        return Err(format!(
            "OpenPGP library ABI {found}, expected {ABI_VERSION}"
        ));
    }

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
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "{LIBRARY_PATH_ENV} is set to {}, which is not an existing file",
            path.display()
        ));
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

/// Search order after `ALCEDO_PGP_LIB`: the installed package layout, then
/// next to the executable, then one level up (cargo test binaries live in
/// `target/<profile>/deps/`).
fn bundled_candidates(exe_dir: &Path) -> [PathBuf; 3] {
    [
        exe_dir
            .join("../lib/alcedo")
            .join(INSTALLED_LIBRARY_FILE_NAME),
        exe_dir.join(LIBRARY_FILE_NAME),
        exe_dir.join("..").join(LIBRARY_FILE_NAME),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_candidates_follow_the_documented_order() {
        let candidates = bundled_candidates(Path::new("/opt/alcedo/bin"));
        assert_eq!(
            candidates,
            [
                PathBuf::from("/opt/alcedo/bin/../lib/alcedo/libalcedo_pgp_ffi.so.1"),
                PathBuf::from("/opt/alcedo/bin/libalcedo_pgp_ffi.so"),
                PathBuf::from("/opt/alcedo/bin/../libalcedo_pgp_ffi.so"),
            ]
        );
    }
}
