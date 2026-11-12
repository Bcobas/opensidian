// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::c_void;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::path::PathBuf;
use std::ptr::null;

use ::windows::core::{w, HRESULT};
use ::windows::Win32::{
    Foundation::{ERROR_CANCELLED, HWND},
    System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED},
    UI::Shell::{Common::COMDLG_FILTERSPEC, FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
        FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH},
};

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> RawHandle;
    fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        parent: *const c_void, operation: *const u16, file: *const u16,
        parameters: *const u16, directory: *const u16, show: i32,
    ) -> isize;
}

pub fn process_alive(pid: u32) -> bool {
    use std::os::windows::io::AsRawHandle;
    // SYNCHRONIZE lets us query whether the process handle has been signaled.
    let handle = unsafe { OpenProcess(0x100000, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    (unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) }) == 258 // WAIT_TIMEOUT
}

pub fn open_url(url: &str) -> Result<(), String> {
    if url.contains('\0') {
        return Err("invalid URL".into());
    }
    let target: Vec<u16> = url.encode_utf16().chain([0]).collect();
    // ShellExecute dispatches the already allowlisted URL to its registered
    // handler. The parameters argument is null, so it cannot add arguments.
    let result = unsafe { ShellExecuteW(null(), null(), target.as_ptr(), null(), null(), 1) };
    if result <= 32 {
        return Err(format!("could not open link (Windows shell error {result})"));
    }
    Ok(())
}

pub enum PackagePicker {
    Archive,
    VaultParent,
}

/// Show a modal picker on the window's UI thread.
pub fn pick_package_path(owner: HWND, picker: PackagePicker) -> ::windows::core::Result<Option<PathBuf>> {
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // This guard is created only after a successful initialization on this thread.
            unsafe { CoUninitialize() };
        }
    }

    // The caller runs this on the window thread. Shell objects and their path
    // buffers are used and released in this apartment before returning.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = Apartment;
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let mut options = dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_PATHMUSTEXIST;
        match picker {
            PackagePicker::Archive => {
                dialog.SetTitle(w!("Open portable package"))?;
                dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                    pszName: w!("Opensidian packages (*.opensidian.zip)"),
                    pszSpec: w!("*.opensidian.zip"),
                }])?;
                options |= FOS_FILEMUSTEXIST;
            }
            PackagePicker::VaultParent => {
                dialog.SetTitle(w!("Choose where to save the vault"))?;
                dialog.SetOkButtonLabel(w!("Save here"))?;
                options = (options & !FOS_FILEMUSTEXIST) | FOS_PICKFOLDERS;
            }
        }
        dialog.SetOptions(options)?;
        match dialog.Show(Some(owner)) {
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
            other => other?,
        }

        let item = dialog.GetResult()?;
        let wide = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(std::ffi::OsString::from_wide(wide.as_wide()));
        CoTaskMemFree(Some(wide.0.cast()));
        Ok(Some(path))
    }
}
