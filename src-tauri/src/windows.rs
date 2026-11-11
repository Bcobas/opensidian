// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::c_void;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::null;

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
