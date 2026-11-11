// SPDX-License-Identifier: GPL-3.0-or-later
// opensidian, a vault-compatible markdown notes app.
// Copyright (C) 2026 Jendrik Poloczek
/* opensidian_lib — the PURE parse/normalize modules, split out of the bin so
   fuzz/ (cargo-fuzz) can link them without the tauri runtime. No tauri, no
   window, no global app state lives here. main.rs re-imports each module at
   its crate root (`use opensidian_lib::vaultfs;`), so every existing
   `crate::vaultfs::…` path in the bin resolves to the same code it did before:
   the split moves compilation units, not behaviour. Add a module here ONLY if
   it is tauri-free and a fuzz target needs it. */
#![deny(unused_must_use)]

pub mod canvasfs;
pub mod datefmt;
pub mod graphq;
pub mod index;
pub mod outline;
pub mod vaultfs;

/// Vault-relative names use Markdown path separators on every desktop.
pub fn vault_path_string(path: &std::path::Path) -> String {
    #[cfg(windows)]
    return path.to_string_lossy().replace('\\', "/");
    #[cfg(not(windows))]
    path.to_string_lossy().into_owned()
}

pub fn os_random_hex() -> std::io::Result<String> {
    let mut bytes = [0u8; 8];
    #[cfg(not(windows))]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    }
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(algorithm: *mut std::ffi::c_void, buffer: *mut u8, len: u32, flags: u32) -> i32;
        }
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG takes a null algorithm handle.
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, 2) };
        if status < 0 {
            return Err(std::io::Error::other(format!("BCryptGenRandom failed: {status:#x}")));
        }
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
