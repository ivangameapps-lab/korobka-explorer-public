use std::path::PathBuf;

#[derive(Clone)]
pub struct DriveInfo {
    pub root: PathBuf,
    pub letter: char,
    pub label: String,
    pub kind: &'static str,
    pub total: u64,
    pub free: u64,
}

impl DriveInfo {
    pub fn caption(&self) -> String {
        if self.label.is_empty() {
            format!("{}:", self.letter)
        } else {
            format!("{}: {}", self.letter, self.label)
        }
    }
}

#[cfg(windows)]
pub fn enumerate() -> Vec<DriveInfo> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };

    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();

    for letter in b'A'..=b'Z' {
        if mask & (1u32 << (letter - b'A')) == 0 {
            continue;
        }
        let letter = letter as char;
        let wide: Vec<u16> = format!("{letter}:\\")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
        let kind = match drive_type {
            2 => "съёмный",
            3 => "локальный",
            4 => "сетевой",
            5 => "CD/DVD",
            6 => "RAM-диск",
            _ => "диск",
        };

        let mut label_buf = [0u16; 256];
        let mut fs_buf = [0u16; 64];
        let ok = unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                label_buf.as_mut_ptr(),
                label_buf.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_buf.as_mut_ptr(),
                fs_buf.len() as u32,
            )
        };
        let label = if ok != 0 {
            utf16z(&label_buf)
        } else {
            String::new()
        };

        let mut free = 0u64;
        let mut total = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), std::ptr::null_mut(), &mut total, &mut free)
        };
        if ok == 0 {
            total = 0;
            free = 0;
        }

        drives.push(DriveInfo {
            root: PathBuf::from(format!("{letter}:\\")),
            letter,
            label,
            kind,
            total,
            free,
        });
    }

    drives
}

#[cfg(windows)]
fn utf16z(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(not(windows))]
pub fn enumerate() -> Vec<DriveInfo> {
    Vec::new()
}
