use std::path::PathBuf;

#[cfg(windows)]
mod imp {
    use super::PathBuf;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::ptr;
    use std::slice;
    use std::thread::sleep;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
    };

    const CF_HDROP: u32 = 15;
    const DROPEFFECT_COPY: u32 = 1;
    const DROPEFFECT_MOVE: u32 = 2;
    const PREFERRED_DROP_EFFECT: &[u16] = &[
        b'P' as u16,
        b'r' as u16,
        b'e' as u16,
        b'f' as u16,
        b'e' as u16,
        b'r' as u16,
        b'r' as u16,
        b'e' as u16,
        b'd' as u16,
        b' ' as u16,
        b'D' as u16,
        b'r' as u16,
        b'o' as u16,
        b'p' as u16,
        b' ' as u16,
        b'E' as u16,
        b'f' as u16,
        b'f' as u16,
        b'e' as u16,
        b'c' as u16,
        b't' as u16,
        0,
    ];

    #[repr(C)]
    struct DropFiles {
        p_files: u32,
        pt_x: i32,
        pt_y: i32,
        f_nc: i32,
        f_wide: i32,
    }

    fn open_clipboard() -> bool {
        for _ in 0..8 {
            if unsafe { OpenClipboard(ptr::null_mut::<std::ffi::c_void>() as HWND) } != 0 {
                return true;
            }
            sleep(Duration::from_millis(10));
        }
        false
    }

    fn put_effect(format: u32, cut: bool) -> bool {
        let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, 4) };
        if hmem.is_null() {
            return false;
        }
        let data = unsafe { GlobalLock(hmem) } as *mut u32;
        if data.is_null() {
            unsafe {
                GlobalFree(hmem);
            }
            return false;
        }
        unsafe {
            *data = if cut {
                DROPEFFECT_MOVE
            } else {
                DROPEFFECT_COPY
            };
            GlobalUnlock(hmem);
        }
        let ok = unsafe { !SetClipboardData(format, hmem).is_null() };
        if !ok {
            unsafe {
                GlobalFree(hmem);
            }
        }
        ok
    }

    pub fn set_files(paths: &[PathBuf], cut: bool) -> bool {
        if paths.is_empty() {
            return false;
        }

        let mut names = Vec::new();
        for path in paths {
            names.extend(path.as_os_str().encode_wide());
            names.push(0);
        }
        names.push(0);

        let header_size = std::mem::size_of::<DropFiles>();
        let bytes = header_size + names.len() * 2;
        let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes) };
        if hmem.is_null() {
            return false;
        }
        let data = unsafe { GlobalLock(hmem) } as *mut u8;
        if data.is_null() {
            unsafe {
                GlobalFree(hmem);
            }
            return false;
        }

        unsafe {
            let header = DropFiles {
                p_files: header_size as u32,
                pt_x: 0,
                pt_y: 0,
                f_nc: 0,
                f_wide: 1,
            };
            ptr::copy_nonoverlapping(&header as *const DropFiles as *const u8, data, header_size);
            ptr::copy_nonoverlapping(
                names.as_ptr() as *const u8,
                data.add(header_size),
                names.len() * 2,
            );
            GlobalUnlock(hmem);
        }

        if !open_clipboard() {
            unsafe {
                GlobalFree(hmem);
            }
            return false;
        }

        let mut ok = false;
        unsafe {
            if EmptyClipboard() != 0 && !SetClipboardData(CF_HDROP, hmem).is_null() {
                ok = true;
                let format = RegisterClipboardFormatW(PREFERRED_DROP_EFFECT.as_ptr());
                if format != 0 {
                    put_effect(format, cut);
                }
            }
            CloseClipboard();
        }
        if !ok {
            unsafe {
                GlobalFree(hmem);
            }
        }
        ok
    }

    fn read_effect() -> bool {
        let format = unsafe { RegisterClipboardFormatW(PREFERRED_DROP_EFFECT.as_ptr()) };
        if format == 0 {
            return false;
        }
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return false;
        }
        let data = unsafe { GlobalLock(handle) } as *const u32;
        if data.is_null() {
            return false;
        }
        let effect = unsafe { *data };
        unsafe {
            GlobalUnlock(handle);
        }
        effect == DROPEFFECT_MOVE
    }

    pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
        if unsafe { IsClipboardFormatAvailable(CF_HDROP) } == 0 {
            return None;
        }
        if !open_clipboard() {
            return None;
        }

        let mut result = None;
        unsafe {
            let handle: HANDLE = GetClipboardData(CF_HDROP);
            if !handle.is_null() {
                let size = GlobalSize(handle as HGLOBAL);
                let data = GlobalLock(handle as HGLOBAL) as *const u8;
                if !data.is_null() {
                    let header_size = std::mem::size_of::<DropFiles>();
                    if size >= header_size {
                        let header = &*(data as *const DropFiles);
                        let offset = header.p_files as usize;
                        if header.f_wide != 0 && offset >= header_size && offset + 2 <= size {
                            let units = (size - offset) / 2;
                            let raw = slice::from_raw_parts(data.add(offset) as *const u16, units);
                            let mut paths = Vec::new();
                            let mut start = 0;
                            for (index, unit) in raw.iter().enumerate() {
                                if *unit == 0 {
                                    if index == start {
                                        break;
                                    }
                                    paths.push(PathBuf::from(OsString::from_wide(
                                        &raw[start..index],
                                    )));
                                    start = index + 1;
                                }
                            }
                            if !paths.is_empty() {
                                result = Some((paths, read_effect()));
                            }
                        }
                    }
                    GlobalUnlock(handle as HGLOBAL);
                }
            }
            CloseClipboard();
        }
        result
    }

    pub fn clear() -> bool {
        if !open_clipboard() {
            return false;
        }
        let ok = unsafe { EmptyClipboard() != 0 };
        unsafe {
            CloseClipboard();
        }
        ok
    }
}

#[cfg(windows)]
pub use imp::{clear, get_files, set_files};

#[cfg(not(windows))]
pub fn set_files(_paths: &[PathBuf], _cut: bool) -> bool {
    false
}

#[cfg(not(windows))]
pub fn get_files() -> Option<(Vec<PathBuf>, bool)> {
    None
}

#[cfg(not(windows))]
pub fn clear() -> bool {
    false
}
