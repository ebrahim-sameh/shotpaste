//! Cross-platform single-instance guard.
//!
//! Two watchers in the same user session fight over the clipboard: each
//! `notify` event fires once per process, and both race to call
//! `clipboard::write_png` on the same file. On Windows that results in
//! `ERROR_ACCESS_DENIED` cascades against itself; on macOS/Linux it just
//! double-pushes which is wasteful but harmless. Either way, prevent it.
//!
//! - **Windows**: `CreateMutexW(L"Local\\Shotpaste")`. The `Local\` namespace
//!   prefix scopes the mutex to the current session, so two users on the
//!   same machine each get their own instance.
//! - **macOS / Linux**: exclusive `flock` on `<cache_dir>/shotpaste/shotpaste.lock`.
//!
//! The guard releases automatically when dropped (mutex handle closed, file
//! lock released). Hold it in `main` for the lifetime of the process.

use anyhow::Result;

#[cfg(not(target_os = "windows"))]
pub use unix_impl::Guard;
#[cfg(target_os = "windows")]
pub use windows_impl::Guard;

/// Try to acquire the single-instance lock. Returns the guard on success;
/// `Err` if another shotpaste instance is already running in this session.
pub fn acquire() -> Result<Guard> {
    Guard::acquire()
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use anyhow::{Result, bail};
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    pub struct Guard {
        handle: HANDLE,
    }

    impl Guard {
        pub fn acquire() -> Result<Guard> {
            let name: Vec<u16> = OsStr::new("Local\\Shotpaste-Watcher")
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `name` is a valid null-terminated UTF-16 string; we
            // pass a null SECURITY_ATTRIBUTES and request non-initial-owner
            // so the call cannot fail with anything other than the
            // already-exists error we explicitly handle below.
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if handle.is_null() {
                let code = unsafe { GetLastError() };
                bail!("CreateMutexW failed (code {code})");
            }
            // `CreateMutexW` returns the existing handle if the mutex
            // already exists; the *only* way to distinguish "freshly
            // created" from "already existed" is `GetLastError()`
            // immediately after the call.
            let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if already {
                unsafe { CloseHandle(handle) };
                bail!(
                    "another shotpaste instance is already running in this session.\n  \
                     If this is unexpected, check Task Manager for `shotpaste.exe` or \
                     `wscript.exe` running the Scheduled Task."
                );
            }
            Ok(Guard { handle })
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: `handle` came from `CreateMutexW` and is only closed
            // here, exactly once.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod unix_impl {
    use anyhow::{Context, Result};
    use fs2::FileExt;
    use std::fs::File;
    use std::path::PathBuf;

    pub struct Guard {
        // Keep the file alive — flock is released on close.
        _file: File,
    }

    impl Guard {
        pub fn acquire() -> Result<Guard> {
            let path = lock_path()?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            let file = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .with_context(|| format!("failed to open lock file {}", path.display()))?;
            file.try_lock_exclusive().map_err(|e| {
                anyhow::anyhow!(
                    "another shotpaste instance appears to be running (lock {} held: {e})",
                    path.display()
                )
            })?;
            Ok(Guard { _file: file })
        }
    }

    fn lock_path() -> Result<PathBuf> {
        let cache = dirs::cache_dir().context("could not resolve cache dir")?;
        Ok(cache.join("shotpaste").join("shotpaste.lock"))
    }
}
