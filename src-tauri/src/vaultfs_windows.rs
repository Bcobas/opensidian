// SPDX-License-Identifier: GPL-3.0-or-later

use super::{fill, refused, same_bytes, ImportErr, Imported, PStat, SrcFate};
use std::ffi::{c_void, OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::path::{Component, Path, PathBuf};
use std::ptr::{null, null_mut};

const OPEN: u32 = 1;
const CREATE: u32 = 2;
const OPEN_IF: u32 = 3;
const LIST_DIRECTORY: u32 = 1;
const READ_DATA: u32 = 1;
const WRITE_DATA: u32 = 2;
const APPEND_DATA: u32 = 4;
const TRAVERSE: u32 = 0x20;
const READ_ATTRIBUTES: u32 = 0x80;
const DELETE: u32 = 0x10000;
const SYNCHRONIZE: u32 = 0x100000;
const DIRECTORY: u32 = 1;
const SYNCHRONOUS: u32 = 0x20;
const OPEN_REPARSE_POINT: u32 = 0x00200000;
const BACKUP_SEMANTICS: u32 = 0x02000000;
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE: u32 = 0x400;
const SHARE_READ: u32 = 1;
const SHARE_WRITE: u32 = 2;
const SHARE_DELETE: u32 = 4;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: RawHandle,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const c_void,
    security_quality_of_service: *const c_void,
}

#[repr(C)]
struct IoStatus {
    status: usize,
    information: usize,
}

#[repr(C)]
struct RenameInformation {
    replace: u8,
    directory: RawHandle,
    name_length: u32,
    name: [u16; 1],
}

#[link(name = "ntdll")]
extern "system" {
    fn NtCreateFile(
        handle: *mut RawHandle, access: u32, object: *const ObjectAttributes,
        status: *mut IoStatus, size: *const i64, attributes: u32, share: u32,
        disposition: u32, options: u32, ea: *const c_void, ea_length: u32,
    ) -> i32;
    fn NtSetInformationFile(
        handle: RawHandle, status: *mut IoStatus, information: *const c_void,
        length: u32, class: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandleEx(
        handle: RawHandle, class: i32, data: *mut c_void, length: u32,
    ) -> i32;
}

fn status(code: i32) -> io::Result<()> {
    if code >= 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(code) } as i32))
    }
}

// A single NT path component, without stream syntax, Win32 aliases, or separators.
pub(super) fn valid_component(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else { return false };
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            })
        });
    !name.is_empty() && !name.ends_with(['.', ' ']) && !reserved
        && !name.chars().any(|ch| ch < ' ' || "<>:\"/\\|?*".contains(ch))
}

fn component(name: &OsStr) -> io::Result<Vec<u16>> {
    if !valid_component(name) {
        return Err(refused(Path::new(name), "invalid Windows path component"));
    }
    Ok(name.encode_wide().collect())
}

fn split(path: &Path) -> io::Result<Vec<&OsStr>> {
    path.components().map(|part| match part {
        Component::Normal(name) if valid_component(name) => Ok(name),
        _ => Err(refused(path, "not a plain vault-relative path")),
    }).collect()
}

fn check(file: &File, name: &OsStr) -> io::Result<Metadata> {
    let meta = file.metadata()?;
    if meta.file_attributes() & ATTR_REPARSE != 0 {
        return Err(refused(Path::new(name), "reparse point in vault path"));
    }
    Ok(meta)
}

fn open_at(
    parent: &File, name: &OsStr, access: u32, share: u32, disposition: u32, options: u32,
) -> io::Result<File> {
    let wide = component(name)?;
    let length = u16::try_from(wide.len() * 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component too long"))?;
    let unicode = UnicodeString { length, maximum_length: length, buffer: wide.as_ptr() };
    let object = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &unicode,
        attributes: 0x40, // OBJ_CASE_INSENSITIVE
        security_descriptor: null(),
        security_quality_of_service: null(),
    };
    let mut handle = null_mut();
    let mut io = IoStatus { status: 0, information: 0 };
    // The counted name contains one component. The parent handle stays live for
    // the entire call, and an opened reparse point is refused before any I/O.
    status(unsafe {
        NtCreateFile(
            &mut handle, access | READ_ATTRIBUTES | SYNCHRONIZE, &object, &mut io,
            null(), 0, share, disposition, options | SYNCHRONOUS | OPEN_REPARSE_POINT,
            null(), 0,
        )
    })?;
    let file = unsafe { File::from_raw_handle(handle) };
    check(&file, name)?;
    Ok(file)
}

fn directory_at(parent: &File, name: &OsStr, disposition: u32) -> io::Result<File> {
    let options = if disposition == CREATE { DIRECTORY } else { 0 };
    let dir = open_at(parent, name, LIST_DIRECTORY | TRAVERSE, SHARE_READ | SHARE_WRITE, disposition, options)?;
    if !dir.metadata()?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "expected a directory"));
    }
    Ok(dir)
}

fn regular(file: File) -> io::Result<File> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::IsADirectory, "expected a regular file"));
    }
    Ok(file)
}

fn rename(file: &File, parent: Option<&File>, name: &OsStr, replace: bool) -> io::Result<()> {
    let name = component(name)?;
    let start = std::mem::offset_of!(RenameInformation, name);
    let length = size_of::<RenameInformation>() + name.len() * 2;
    let mut storage = vec![0usize; length.div_ceil(size_of::<usize>())];
    let info = storage.as_mut_ptr().cast::<RenameInformation>();
    let mut io = IoStatus { status: 0, information: 0 };
    // The zeroed buffer is aligned for the header and sized for its trailing name.
    unsafe {
        (*info).replace = u8::from(replace);
        (*info).directory = parent.map_or(null_mut(), File::as_raw_handle);
        (*info).name_length = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), info.cast::<u8>().add(start).cast(), name.len());
        status(NtSetInformationFile(file.as_raw_handle(), &mut io, info.cast(), length as u32, 10))
    }
}

fn remove(file: &File) -> io::Result<()> {
    let mut io = IoStatus { status: 0, information: 0 };
    let delete = 1u8;
    // FileDispositionInformation marks the opened entry for deletion on close.
    status(unsafe { NtSetInformationFile(file.as_raw_handle(), &mut io, (&delete as *const u8).cast(), 1, 13) })
}

struct Stage {
    file: File,
    committed: bool,
}

impl Stage {
    fn new(parent: &File) -> io::Result<Self> {
        let name = format!(".opensidian-{}~", crate::os_random_hex()?);
        let file = open_at(parent, OsStr::new(&name), READ_DATA | WRITE_DATA | DELETE, SHARE_READ, CREATE, 0)?;
        Ok(Self { file, committed: false })
    }

    fn commit(&mut self, name: &OsStr, replace: bool) -> io::Result<()> {
        rename(&self.file, None, name, replace)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if !self.committed {
            let _ = remove(&self.file);
        }
    }
}

pub struct Dir {
    // Denying delete sharing pins every ancestor until the operation completes.
    handles: Vec<File>,
}

impl Dir {
    fn handle(&self) -> &File {
        self.handles.last().expect("a directory has a root handle")
    }
}

pub struct Vault {
    root: Dir,
}

impl Vault {
    pub fn open(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let mut volume = PathBuf::new();
        let mut path = root.components();
        for _ in 0..2 {
            volume.push(path.next().ok_or_else(|| refused(&root, "invalid volume"))?);
        }
        let file = OpenOptions::new().read(true)
            .share_mode(SHARE_READ | SHARE_WRITE)
            .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
            .open(&volume)?;
        if !check(&file, volume.as_os_str())?.is_dir() {
            return Err(refused(&root, "invalid volume"));
        }
        let mut vault = Self { root: Dir { handles: vec![file] } };
        for entry in path {
            let Component::Normal(name) = entry else { return Err(refused(&root, "invalid root")); };
            let next = directory_at(vault.root.handle(), name, OPEN)?;
            vault.root.handles.push(next);
        }
        Ok(vault)
    }

    fn walk(&self, names: &[&OsStr], create: bool) -> io::Result<Dir> {
        let mut dir = Dir { handles: vec![self.root.handle().try_clone()?] };
        for &name in names {
            let parent = dir.handle();
            let next = match directory_at(parent, name, OPEN) {
                Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                    match directory_at(parent, name, CREATE) {
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => directory_at(parent, name, OPEN)?,
                        result => result?,
                    }
                }
                result => result?,
            };
            dir.handles.push(next);
        }
        Ok(dir)
    }

    pub fn dir(&self, rel: &Path, create: bool) -> io::Result<Dir> {
        self.walk(&split(rel)?, create)
    }

    pub fn mkdir_p(&self, rel: &Path) -> io::Result<()> {
        self.dir(rel, true).map(drop)
    }

    fn parent<'a>(&self, rel: &'a Path, create: bool) -> io::Result<(Dir, &'a OsStr)> {
        let names = split(rel)?;
        let (&name, parents) = names.split_last().ok_or_else(|| refused(rel, "empty path"))?;
        Ok((self.walk(parents, create)?, name))
    }

    pub fn open_read(&self, rel: &Path) -> io::Result<File> {
        let (parent, name) = self.parent(rel, false)?;
        regular(open_at(parent.handle(), name, READ_DATA, SHARE_READ | SHARE_WRITE | SHARE_DELETE, OPEN, 0)?)
    }

    pub fn is_file(&self, rel: &Path) -> bool {
        self.open_read(rel).is_ok()
    }

    fn write_in(&self, dir: &Dir, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        let dir = dir.handle();
        match open_at(dir, name, 0, SHARE_READ | SHARE_WRITE | SHARE_DELETE, OPEN, 0) {
            Ok(file) => { regular(file)?; }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut stage = Stage::new(dir)?;
        stage.file.write_all(bytes)?;
        stage.file.sync_all()?;
        stage.commit(name, true)
    }

    pub fn write_atomic(&self, rel: &Path, bytes: &[u8], _durable: bool) -> io::Result<()> {
        let (parent, name) = self.parent(rel, true)?;
        self.write_in(&parent, name, bytes)
    }

    /// Stage the save first, then compare against the snapshot immediately before replacing it.
    pub fn replace_if_unchanged(&self, rel: &Path, expected: &[u8], bytes: &[u8]) -> io::Result<bool> {
        let (parent, name) = self.parent(rel, false)?;
        let dir = parent.handle();
        let mut stage = Stage::new(dir)?;
        stage.file.write_all(bytes)?;
        stage.file.sync_all()?;
        let current = match open_at(dir, name, READ_DATA, SHARE_READ, OPEN, 0) {
            Ok(file) => regular(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut original = Vec::new();
        current.take(expected.len() as u64 + 1).read_to_end(&mut original)?;
        if original != expected {
            return Ok(false);
        }
        stage.commit(name, true)?;
        Ok(true)
    }

    pub fn write_new_atomic(&self, rel: &Path, bytes: &[u8]) -> io::Result<()> {
        let (parent, name) = self.parent(rel, false)?;
        let mut stage = Stage::new(parent.handle())?;
        stage.file.write_all(bytes)?;
        stage.file.sync_all()?;
        stage.commit(name, false)
    }

    pub fn create_new(&self, rel: &Path, parents: bool) -> io::Result<File> {
        let (parent, name) = self.parent(rel, parents)?;
        open_at(parent.handle(), name, WRITE_DATA, SHARE_READ, CREATE, 0)
    }

    pub fn unlink(&self, rel: &Path) -> io::Result<()> {
        self.p_remove(rel, false)
    }

    pub fn move_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (source, name) = self.parent(from, false)?;
        let file = regular(open_at(source.handle(), name, DELETE, SHARE_READ, OPEN, 0)?)?;
        let (destination, name) = self.parent(to, true)?;
        rename(&file, Some(destination.handle()), name, false)
    }

    pub fn trash(&self, rel: &Path, dir: &Path, stem: &str, ext: &str) -> io::Result<PathBuf> {
        let (source, name) = self.parent(rel, false)?;
        let file = regular(open_at(source.handle(), name, DELETE, SHARE_READ, OPEN, 0)?)?;
        let destination = self.dir(dir, true)?;
        for i in 0..10_000 {
            let name = if i == 0 { format!("{stem}.{ext}") } else { format!("{stem}.{i}.{ext}") };
            match rename(&file, Some(destination.handle()), OsStr::new(&name), false) {
                Ok(()) => return Ok(dir.join(name)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name in the trash"))
    }

    pub fn import_move(
        &self, src: &Path, dir: &Path, names: &mut dyn Iterator<Item = String>,
        cap: u64, magic: &[u8], keep_src: bool,
    ) -> Result<Imported, ImportErr> {
        use std::io::{Seek, SeekFrom};
        let map = ImportErr::Io;
        let parent = src.parent().ok_or(ImportErr::NotRegular)?;
        let name = src.file_name().ok_or(ImportErr::NotRegular)?;
        let source = Self::open(parent).map_err(ImportErr::OpenSrc)?;
        let open_source = |access| open_at(source.root.handle(), name, access, SHARE_READ, OPEN, 0);
        let (mut input, can_remove) = if keep_src {
            (open_source(READ_DATA).map_err(ImportErr::OpenSrc)?, false)
        } else {
            match open_source(READ_DATA | DELETE) {
                Ok(file) => (file, true),
                Err(_) => (open_source(READ_DATA).map_err(ImportErr::OpenSrc)?, false),
            }
        };
        if !input.metadata().map_err(map)?.is_file() {
            return Err(ImportErr::NotRegular);
        }
        let size = input.metadata().map_err(map)?.len();
        if size > cap {
            return Err(ImportErr::TooBig(size));
        }
        let mut head = vec![0; magic.len()];
        if fill(&mut input, &mut head).map_err(map)? != magic.len() || head != magic {
            return Err(ImportErr::BadMagic);
        }
        input.seek(SeekFrom::Start(0)).map_err(map)?;
        let destination = self.dir(dir, false).map_err(map)?;
        let mut stage = Stage::new(destination.handle()).map_err(map)?;
        let copied = io::copy(&mut (&mut input).take(cap.saturating_add(1)), &mut stage.file).map_err(map)?;
        if copied > cap {
            return Err(ImportErr::TooBig(copied));
        }
        stage.file.sync_all().map_err(map)?;
        if copied != size || !same_bytes(&mut input, &mut stage.file).map_err(map)? {
            return Err(ImportErr::Changed);
        }
        for name in names {
            match stage.commit(OsStr::new(&name), false) {
                Ok(()) => {
                    let src = if keep_src {
                        SrcFate::Copied
                    } else if can_remove {
                        match remove(&input) {
                            Ok(()) => SrcFate::Removed,
                            Err(e) => SrcFate::Kept(e.to_string()),
                        }
                    } else {
                        SrcFate::Kept("source could not be opened for deletion".into())
                    };
                    return Ok(Imported { name, src, how: "copy" });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(map(e)),
            }
        }
        Err(ImportErr::NoFreeName)
    }

    pub fn p_stat(&self, rel: &Path) -> io::Result<PStat> {
        let meta = if rel.as_os_str().is_empty() {
            self.root.handle().metadata()?
        } else {
            let (parent, name) = self.parent(rel, false)?;
            open_at(parent.handle(), name, 0, SHARE_READ | SHARE_WRITE | SHARE_DELETE, OPEN, 0)?.metadata()?
        };
        let ms = |ticks: u64| ((ticks as i128 - 116_444_736_000_000_000) / 10_000) as i64;
        Ok(PStat {
            kind: if meta.is_dir() { "folder" } else { "file" },
            size: meta.len(),
            mtime_ms: ms(meta.last_write_time()),
            ctime_ms: ms(meta.creation_time()),
        })
    }

    pub fn p_read(&self, rel: &Path, cap: u64) -> io::Result<Vec<u8>> {
        let file = self.open_read(rel)?;
        let mut bytes = Vec::new();
        file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > cap {
            return Err(io::Error::other("file too large"));
        }
        Ok(bytes)
    }

    pub fn p_write(&self, rel: &Path, bytes: &[u8], append: bool) -> io::Result<()> {
        let (parent, name) = self.parent(rel, false)?;
        if append {
            let mut file = regular(open_at(parent.handle(), name, APPEND_DATA, SHARE_READ | SHARE_WRITE, OPEN_IF, 0)?)?;
            file.write_all(bytes)
        } else {
            self.write_in(&parent, name, bytes)
        }
    }

    pub fn p_list(&self, rel: &Path) -> io::Result<Vec<(String, &'static str)>> {
        let dir = if rel.as_os_str().is_empty() {
            self.root.handle().try_clone()?
        } else {
            let (parent, name) = self.parent(rel, false)?;
            directory_at(parent.handle(), name, OPEN)?
        };
        list(&dir)
    }

    pub fn p_mkdir(&self, rel: &Path, recursive: bool) -> io::Result<()> {
        if recursive {
            return self.mkdir_p(rel);
        }
        let (parent, name) = self.parent(rel, false)?;
        directory_at(parent.handle(), name, CREATE).map(drop)
    }

    pub fn p_remove(&self, rel: &Path, dir: bool) -> io::Result<()> {
        let (parent, name) = self.parent(rel, false)?;
        let file = open_at(parent.handle(), name, DELETE, SHARE_READ | SHARE_WRITE, OPEN, 0)?;
        if file.metadata()?.is_dir() != dir {
            let kind = if dir { io::ErrorKind::NotADirectory } else { io::ErrorKind::IsADirectory };
            return Err(io::Error::new(kind, "unexpected file type"));
        }
        remove(&file)
    }

    pub fn p_rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (source, name) = self.parent(from, false)?;
        let file = open_at(source.handle(), name, DELETE, SHARE_READ, OPEN, 0)?;
        let (destination, name) = self.parent(to, false)?;
        rename(&file, Some(destination.handle()), name, false)
    }

    pub fn p_copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        let bytes = self.p_read(from, super::PLUGIN_READ_CAP)?;
        self.p_write(to, &bytes, false)
    }

    pub fn p_chmod(&self, _rel: &Path, _mode: u32) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "POSIX chmod is unavailable on Windows"))
    }
}

fn list(dir: &File) -> io::Result<Vec<(String, &'static str)>> {
    let mut storage = [0u64; 8192];
    let length = size_of_val(&storage);
    let mut class = 15; // FileFullDirectoryRestartInfo
    let mut entries = Vec::new();
    loop {
        let ok = unsafe { GetFileInformationByHandleEx(dir.as_raw_handle(), class, storage.as_mut_ptr().cast(), length as u32) };
        if ok == 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(18) { Ok(entries) } else { Err(e) };
        }
        class = 14; // FileFullDirectoryInfo
        // FILE_FULL_DIR_INFO is 8-byte aligned and contains a UTF-16 name at byte 68.
        let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), length) };
        let mut at = 0;
        loop {
            let u32_at = |n| u32::from_le_bytes(bytes[at + n..at + n + 4].try_into().unwrap());
            let next = u32_at(0) as usize;
            let attributes = u32_at(56);
            let name = &bytes[at + 68..at + 68 + u32_at(60) as usize];
            if attributes & ATTR_REPARSE == 0 {
                let name: Vec<_> = name.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
                let name = OsString::from_wide(&name);
                if name != "." && name != ".." {
                    let kind = if attributes & ATTR_DIRECTORY == 0 { "file" } else { "folder" };
                    entries.push((name.to_string_lossy().into_owned(), kind));
                }
            }
            if next == 0 {
                break;
            }
            at += next;
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("opensidian-{}", crate::os_random_hex().unwrap()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_renames_and_trashes_notes() {
        let root = Fixture::new();
        let vault = Vault::open(&root.0).unwrap();
        let first = Path::new("sub/Café.md");
        let second = Path::new("sub/Otra.md");
        vault.write_atomic(first, b"initial", true).unwrap();
        vault.write_atomic(first, b"updated", true).unwrap();
        vault.write_atomic(second, b"existing", false).unwrap();
        assert_eq!(vault.p_read(first, 32).unwrap(), b"updated");
        assert_eq!(vault.move_file(first, second).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        vault.unlink(second).unwrap();
        vault.move_file(first, second).unwrap();
        let trashed = vault.trash(second, Path::new(".trash"), "Otra", "md").unwrap();
        assert_eq!(vault.p_read(&trashed, 32).unwrap(), b"updated");
        assert!(!vault.is_file(second));
        assert_eq!(vault.p_list(Path::new("")).unwrap().len(), 2);
    }

    #[test]
    fn rejects_a_junction_and_named_streams() {
        let root = Fixture::new();
        let outside = Fixture::new();
        let link = root.0.join("junction");
        let result = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:OPENSIDIAN_TEST_LINK -Target $env:OPENSIDIAN_TEST_TARGET | Out-Null"])
            .env("OPENSIDIAN_TEST_LINK", &link)
            .env("OPENSIDIAN_TEST_TARGET", &outside.0)
            .status().unwrap();
        assert!(result.success());
        let vault = Vault::open(&root.0).unwrap();
        std::fs::write(outside.0.join("private.md"), b"outside").unwrap();
        assert!(vault.open_read(Path::new("junction/private.md")).is_err());
        assert!(vault.write_atomic(Path::new("junction/private.md"), b"overwrite", true).is_err());
        assert!(vault.write_atomic(Path::new("ordinary.md:hidden"), b"stream", false).is_err());
        assert_eq!(std::fs::read(outside.0.join("private.md")).unwrap(), b"outside");
        std::fs::remove_dir(link).unwrap();
    }

    #[test]
    fn imports_without_overwriting_or_removing_a_copy_source() {
        let root = Fixture::new();
        let outside = Fixture::new();
        let vault = Vault::open(&root.0).unwrap();
        let input = outside.0.join("input.pdf");
        std::fs::write(&input, b"%PDF-bytes").unwrap();
        vault.write_atomic(Path::new("taken.pdf"), b"existing", true).unwrap();
        let names = || ["taken.pdf".into(), "new.pdf".into()].into_iter();
        let result = vault.import_move(&input, Path::new(""), &mut names(), 64, b"%PDF-", true).unwrap();
        assert_eq!(result.src, SrcFate::Copied);
        assert_eq!(result.name, "new.pdf");
        assert_eq!(vault.p_read(Path::new("new.pdf"), 64).unwrap(), b"%PDF-bytes");
        assert!(input.is_file());
        vault.unlink(Path::new("new.pdf")).unwrap();
        let result = vault.import_move(&input, Path::new(""), &mut names(), 64, b"%PDF-", false).unwrap();
        assert_eq!(result.src, SrcFate::Removed);
        assert!(!input.exists());
    }
}
