// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use ::windows::core::{w, Error, HRESULT, PCWSTR};
use ::windows::Win32::{
    Foundation::{ERROR_CANCELLED, GlobalFree, HANDLE, HWND},
    System::{
        Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED},
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::Shell::{Common::COMDLG_FILTERSPEC, FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
        FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SHOpenFolderAndSelectItems, SHParseDisplayName, SIGDN_FILESYSPATH},
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

struct Apartment;

impl Apartment {
    fn enter() -> ::windows::core::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?; }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    dunce::simplified(path).as_os_str().encode_wide().chain([0]).collect()
}

pub fn reveal_path(path: &Path) -> ::windows::core::Result<()> {
    let _apartment = Apartment::enter()?;
    let path = wide_path(path);
    let mut item = null_mut();
    // With zero child IDs, Explorer selects the parsed item in its parent folder.
    unsafe {
        SHParseDisplayName(PCWSTR(path.as_ptr()), None, &mut item, 0, None)?;
        let result = SHOpenFolderAndSelectItems(item, None, 0);
        CoTaskMemFree(Some(item.cast()));
        result
    }
}

pub fn open_file(owner: HWND, path: &Path) -> Result<(), String> {
    let _apartment = Apartment::enter().map_err(|error| error.to_string())?;
    let path = wide_path(path);
    let result = unsafe { ShellExecuteW(owner.0, w!("open").as_ptr(), path.as_ptr(), null(), null(), 1) };
    match result {
        27 | 31 => Err("Windows has no default app for this file type".into()),
        ..=32 => Err(format!("Could not open the file (Windows shell error {result})")),
        _ => Ok(()),
    }
}

pub fn copy_path(owner: HWND, path: &Path) -> ::windows::core::Result<()> {
    const CF_UNICODETEXT: u32 = 13;
    let path = wide_path(path);
    let bytes = unsafe { std::slice::from_raw_parts(path.as_ptr().cast(), std::mem::size_of_val(path.as_slice())) };
    copy_clipboard(owner, CF_UNICODETEXT, bytes)
}

pub fn graph_bitmap(payload: &[u8]) -> Result<Vec<u8>, String> {
    const HEADER_BYTES: usize = 40; // BITMAPINFOHEADER
    const MAX_PIXELS: usize = 16 * 1024 * 1024;
    let Some((dimensions, pixels)) = payload.split_at_checked(8) else {
        return Err("Invalid graph image".into());
    };
    let width = i32::from_le_bytes(dimensions[..4].try_into().unwrap());
    let height = i32::from_le_bytes(dimensions[4..].try_into().unwrap());
    if width <= 0 || height <= 0 || width as u64 * height as u64 > MAX_PIXELS as u64 ||
        pixels.len() != width as usize * height as usize * 4 {
        return Err("Invalid graph image dimensions or pixel data".into());
    }
    let mut dib = vec![0; HEADER_BYTES + pixels.len()];
    dib[..4].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    dib[4..8].copy_from_slice(&width.to_le_bytes());
    dib[8..12].copy_from_slice(&(-height).to_le_bytes()); // top-down, matching Canvas ImageData
    dib[12..14].copy_from_slice(&1u16.to_le_bytes()); // planes
    dib[14..16].copy_from_slice(&32u16.to_le_bytes()); // uncompressed BGRX
    dib[20..24].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
    for (rgba, bgrx) in pixels.chunks_exact(4).zip(dib[HEADER_BYTES..].chunks_exact_mut(4)) {
        bgrx[..3].copy_from_slice(&[rgba[2], rgba[1], rgba[0]]);
    }
    Ok(dib)
}

pub fn copy_bitmap(owner: HWND, bitmap: &[u8]) -> ::windows::core::Result<()> {
    const CF_DIB: u32 = 8;
    copy_clipboard(owner, CF_DIB, bitmap)
}

fn copy_clipboard(owner: HWND, format: u32, bytes: &[u8]) -> ::windows::core::Result<()> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let result = (|| {
            let buffer = GlobalLock(memory).cast::<u8>();
            if buffer.is_null() {
                return Err(Error::from_win32());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len());
            let _ = GlobalUnlock(memory);
            OpenClipboard(Some(owner))?;
            let result = EmptyClipboard().and_then(|()| SetClipboardData(format, Some(HANDLE(memory.0))));
            let _ = CloseClipboard();
            result.map(|_| ())
        })();
        // SetClipboardData transfers ownership only on success.
        if result.is_err() {
            let _ = GlobalFree(Some(memory));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_top_down_dib_with_bgr_pixels() {
        let payload = [1, 0, 0, 0, 2, 0, 0, 0, 10, 20, 30, 255, 40, 50, 60, 255];
        let image = graph_bitmap(&payload).unwrap();
        assert_eq!(&image[..8], &[40, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(&image[8..12], &(-2i32).to_le_bytes());
        assert_eq!(&image[12..16], &[1, 0, 32, 0]);
        assert_eq!(&image[40..], &[30, 20, 10, 0, 60, 50, 40, 0]);
    }

    #[test]
    fn rejects_truncated_or_oversized_images() {
        assert!(graph_bitmap(&[0; 7]).is_err());
        assert!(graph_bitmap(&[1, 0, 0, 0, 1, 0, 0, 0, 10]).is_err());
        assert!(graph_bitmap(&[255, 255, 255, 127, 255, 255, 255, 127]).is_err());
    }
}

pub enum PackagePicker {
    Archive,
    VaultParent,
    ZipParent,
}

/// Show a modal picker on the window's UI thread.
pub fn pick_package_path(owner: HWND, picker: PackagePicker) -> ::windows::core::Result<Option<PathBuf>> {
    // The caller runs this on the window thread. Shell objects and their path
    // buffers are used and released in this apartment before returning.
    let _apartment = Apartment::enter()?;
    unsafe {
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
            PackagePicker::VaultParent | PackagePicker::ZipParent => {
                let title = match picker {
                    PackagePicker::VaultParent => w!("Choose where to save the vault"),
                    _ => w!("Choose where to save the ZIP copy"),
                };
                dialog.SetTitle(title)?;
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
