#[cfg(unix)]
use anyhow::Context;
use anyhow::{bail, Result};
#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::{
    ffi::{c_char, c_int, c_void, CStr},
    io::{self, Read},
    path::Path,
};

#[link(name = "archive")]
extern "C" {
    fn archive_read_new() -> *mut c_void;
    fn archive_read_support_format_rar(a: *mut c_void) -> c_int;
    fn archive_read_support_format_rar5(a: *mut c_void) -> c_int;
    #[cfg(unix)]
    fn archive_read_open_filename(a: *mut c_void, name: *const c_char, block: usize) -> c_int;
    #[cfg(windows)]
    fn archive_read_open_filename_w(a: *mut c_void, name: *const u16, block: usize) -> c_int;
    fn archive_read_next_header(a: *mut c_void, entry: *mut *mut c_void) -> c_int;
    fn archive_read_data(a: *mut c_void, buffer: *mut c_void, size: usize) -> isize;
    fn archive_read_free(a: *mut c_void) -> c_int;
    fn archive_error_string(a: *mut c_void) -> *const c_char;
    fn archive_entry_pathname_utf8(entry: *mut c_void) -> *const c_char;
    fn archive_entry_filetype(entry: *mut c_void) -> u32;
    fn archive_entry_hardlink(entry: *mut c_void) -> *const c_char;
    fn archive_entry_symlink(entry: *mut c_void) -> *const c_char;
    fn archive_entry_size(entry: *mut c_void) -> i64;
    fn archive_entry_is_encrypted(entry: *mut c_void) -> c_int;
}

pub struct Reader(*mut c_void);
impl Reader {
    fn error(&self) -> String {
        unsafe {
            let p = archive_error_string(self.0);
            if p.is_null() {
                "未知错误".into()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }
    pub fn open(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        let name = CString::new(path.as_os_str().as_bytes()).context("文件路径无效")?;
        #[cfg(windows)]
        let name = {
            let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
            if name.contains(&0) {
                bail!("文件路径无效");
            }
            name.push(0);
            name
        };
        unsafe {
            let reader = Self(archive_read_new());
            if reader.0.is_null() {
                bail!("无法创建 RAR 读取器");
            }
            #[cfg(unix)]
            let open = || archive_read_open_filename(reader.0, name.as_ptr(), 10240);
            #[cfg(windows)]
            let open = || archive_read_open_filename_w(reader.0, name.as_ptr(), 10240);
            if archive_read_support_format_rar(reader.0) != 0
                || archive_read_support_format_rar5(reader.0) != 0
                || open() != 0
            {
                bail!("无法读取 RAR 压缩包：{}", reader.error());
            }
            Ok(reader)
        }
    }
    pub fn next(&mut self) -> Result<Option<super::FileInfo>> {
        unsafe {
            let mut entry = std::ptr::null_mut();
            let status = archive_read_next_header(self.0, &mut entry);
            if status == 1 {
                return Ok(None);
            }
            if status != 0 {
                bail!("无法读取 RAR 条目：{}", self.error());
            }
            let name = archive_entry_pathname_utf8(entry);
            if name.is_null() {
                bail!("RAR 文件名不是有效的 UTF-8");
            }
            let name = CStr::from_ptr(name).to_str()?.to_owned();
            let kind = archive_entry_filetype(entry);
            if (kind != 0o100000 && kind != 0o040000)
                || !archive_entry_hardlink(entry).is_null()
                || !archive_entry_symlink(entry).is_null()
            {
                bail!("暂不支持链接或特殊文件：{name}");
            }
            if archive_entry_is_encrypted(entry) > 0 {
                bail!("暂不支持加密条目：{name}");
            }
            let size = archive_entry_size(entry);
            if size < 0 {
                bail!("RAR 条目大小无效：{name}");
            }
            Ok(Some(super::FileInfo {
                name,
                directory: kind == 0o040000,
                size: size as u64,
                compressed: None,
            }))
        }
    }
}
impl Read for Reader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { archive_read_data(self.0, buffer.as_mut_ptr().cast(), buffer.len()) };
        if n < 0 {
            Err(io::Error::other(self.error()))
        } else {
            Ok(n as usize)
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                archive_read_free(self.0);
            }
        }
    }
}
