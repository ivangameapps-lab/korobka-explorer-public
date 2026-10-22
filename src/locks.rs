use std::path::{Path, PathBuf};

fn localized(ru: String, en: String) -> String {
    if crate::i18n::english() {
        en
    } else {
        ru
    }
}

#[derive(Clone)]
pub struct LockingProcess {
    pub pid: u32,
    pub name: String,
    pub exe: String,
    pub service: String,
    pub critical: bool,
    pub protected: bool,
    pub shell: bool,
}

impl LockingProcess {
    pub fn selectable(&self) -> bool {
        !self.critical && !self.protected && self.service.is_empty()
    }
}

#[derive(Default)]
pub struct LockScan {
    pub processes: Vec<LockingProcess>,
    pub blocked_paths: Vec<PathBuf>,
}

#[cfg(windows)]
pub fn find(paths: &[PathBuf]) -> Result<LockScan, String> {
    use windows_sys::Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS},
        System::RestartManager::{RmGetList, RM_PROCESS_INFO},
    };

    if paths.is_empty() {
        return Ok(LockScan::default());
    }

    let blocked_paths = scan_blocked_files(paths);
    if blocked_paths.is_empty() {
        return Ok(LockScan::default());
    }

    let processes = with_session(&blocked_paths, |session| {
        let mut needed = 0_u32;
        let mut count = 0_u32;
        let mut reboot_reasons = 0_u32;
        let status = unsafe {
            RmGetList(
                session,
                &mut needed,
                &mut count,
                std::ptr::null_mut(),
                &mut reboot_reasons,
            )
        };
        if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
            return Err(localized(
                format!("Restart Manager вернул ошибку {status}"),
                format!("Restart Manager returned error {status}"),
            ));
        }
        if needed == 0 {
            return Ok(Vec::new());
        }

        let mut capacity = needed;
        let infos = loop {
            let mut infos: Vec<RM_PROCESS_INFO> = (0..capacity)
                .map(|_| unsafe { std::mem::zeroed() })
                .collect();
            count = capacity;
            let status = unsafe {
                RmGetList(
                    session,
                    &mut needed,
                    &mut count,
                    infos.as_mut_ptr(),
                    &mut reboot_reasons,
                )
            };
            if status == ERROR_MORE_DATA {
                capacity = needed.max(capacity.saturating_add(8));
                continue;
            }
            if status != ERROR_SUCCESS {
                return Err(localized(
                format!("Restart Manager вернул ошибку {status}"),
                format!("Restart Manager returned error {status}"),
            ));
            }
            infos.truncate(count as usize);
            break infos;
        };

        let current = std::process::id();
        let mut seen = std::collections::BTreeSet::new();
        let mut processes = Vec::new();
        for info in infos {
            let pid = info.Process.dwProcessId;
            if pid == 0 || pid == current || !seen.insert(pid) {
                continue;
            }
            let app_name = wide_string(&info.strAppName);
            let service = wide_string(&info.strServiceShortName);
            let exe = process_image_name(pid);
            let lower_name = app_name.to_ascii_lowercase();
            let shell = exe.eq_ignore_ascii_case("explorer.exe")
                || (exe.is_empty() && lower_name.contains("explorer"));
            let protected = !shell && is_protected_process(&exe, &app_name);
            let name = if !app_name.is_empty() {
                app_name
            } else if !exe.is_empty() {
                exe.clone()
            } else if !service.is_empty() {
                service.clone()
            } else {
                format!("PID {pid}")
            };
            processes.push(LockingProcess {
                pid,
                name,
                exe,
                service,
                critical: info.ApplicationType
                    == windows_sys::Win32::System::RestartManager::RmCritical,
                protected,
                shell,
            });
        }
        processes.sort_by_key(|process| process.name.to_ascii_lowercase());
        Ok(processes)
    })?;

    Ok(LockScan {
        processes,
        blocked_paths,
    })
}

#[cfg(windows)]
pub fn shutdown(paths: &[PathBuf], selected: &[u32]) -> Result<(), String> {
    if selected.is_empty() {
        return Err(crate::i18n::tr("Не выбраны процессы для закрытия").to_owned());
    }
    // Re-check right before closing so a recycled PID is not terminated.
    let scan = find(paths)?;
    let selected = selected
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let processes: Vec<_> = scan
        .processes
        .into_iter()
        .filter(|process| selected.contains(&process.pid))
        .collect();
    if processes.is_empty() {
        return Ok(());
    }

    let mut errors = Vec::new();
    let mut restart_explorer = false;
    for process in processes {
        if !process.selectable() {
            let reason = if process.critical {
                crate::i18n::tr("системный процесс")
            } else if !process.service.is_empty() {
                crate::i18n::tr("служба")
            } else {
                crate::i18n::tr("защищённый процесс")
            };
            errors.push(format!(
                "{} (PID {}): {}",
                process.name, process.pid, reason
            ));
            continue;
        }

        let result = if process.shell {
            force_terminate(process.pid)
        } else {
            close_process(process.pid)
        };
        match result {
            Ok(()) => {
                if process.shell {
                    restart_explorer = true;
                }
            }
            Err(error) => errors.push(format!("{} (PID {}): {error}", process.name, process.pid)),
        }
    }

    if restart_explorer {
        if let Err(error) = restart_shell() {
            errors.push(localized(
                format!("Не удалось перезапустить Проводник: {error}"),
                format!("Could not restart Explorer: {error}"),
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

#[cfg(windows)]
fn scan_blocked_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    use walkdir::WalkDir;

    const MAX_SCAN_FILES: usize = 50_000;
    const MAX_BLOCKED_FILES: usize = 64;

    let mut blocked = Vec::new();
    let mut scanned = 0_usize;
    for path in paths {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if meta.is_dir() && !meta.file_type().is_symlink() {
            for entry in WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if scanned >= MAX_SCAN_FILES || blocked.len() >= MAX_BLOCKED_FILES {
                    break;
                }
                if entry.file_type().is_file() {
                    scanned += 1;
                    if is_delete_blocked(entry.path()) {
                        blocked.push(entry.into_path());
                    }
                }
            }
        } else if meta.is_file() || meta.file_type().is_symlink() {
            scanned += 1;
            if is_delete_blocked(path) {
                blocked.push(path.clone());
            }
        }
        if scanned >= MAX_SCAN_FILES || blocked.len() >= MAX_BLOCKED_FILES {
            break;
        }
    }
    blocked
}

#[cfg(windows)]
fn is_delete_blocked(path: &Path) -> bool {
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, INVALID_HANDLE_VALUE,
        },
        Storage::FileSystem::{
            CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    };

    let wide = wide_path(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let code = std::io::Error::last_os_error().raw_os_error();
        matches!(
            code,
            Some(value)
                if value == ERROR_SHARING_VIOLATION as i32
                    || value == ERROR_LOCK_VIOLATION as i32
        )
    } else {
        unsafe {
            CloseHandle(handle);
        }
        false
    }
}

#[cfg(windows)]
fn is_protected_process(exe: &str, app_name: &str) -> bool {
    let candidate = if !exe.is_empty() { exe } else { app_name };
    let lower = candidate.to_ascii_lowercase();
    const PROTECTED: &[&str] = &[
        "csrss.exe",
        "dwm.exe",
        "fontdrvhost.exe",
        "lsass.exe",
        "runtimebroker.exe",
        "searchhost.exe",
        "services.exe",
        "shellexperiencehost.exe",
        "sihost.exe",
        "smss.exe",
        "startmenuexperiencehost.exe",
        "wininit.exe",
        "winlogon.exe",
    ];
    PROTECTED.iter().any(|name| lower == *name)
}

#[cfg(windows)]
fn process_image_name(pid: u32) -> String {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return String::new();
    }
    let mut buffer = [0_u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 || size == 0 {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[cfg(windows)]
fn close_process(pid: u32) -> Result<(), String> {
    if post_close(pid) && wait_exit(pid, 3_000) {
        Ok(())
    } else {
        force_terminate(pid)
    }
}

#[cfg(windows)]
fn post_close(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{BOOL, HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
        },
    };

    struct Request {
        pid: u32,
        posted: bool,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let request = &mut *(lparam as *mut Request);
        let mut owner = 0_u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut owner);
        }
        if owner == request.pid && unsafe { IsWindowVisible(hwnd) } != 0 {
            unsafe {
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
            request.posted = true;
        }
        1
    }

    let mut request = Request { pid, posted: false };
    unsafe {
        EnumWindows(Some(callback), &mut request as *mut Request as LPARAM);
    }
    request.posted
}

#[cfg(windows)]
fn wait_exit(pid: u32, timeout_ms: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };

    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let status = unsafe { WaitForSingleObject(handle, timeout_ms) };
    unsafe {
        CloseHandle(handle);
    }
    status == WAIT_OBJECT_0
}

#[cfg(windows)]
fn force_terminate(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED},
        System::Threading::{
            OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
            PROCESS_TERMINATE,
        },
    };

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            return Err(crate::i18n::tr("нужны права администратора").to_owned());
        }
        return Err(error.to_string());
    }
    let terminated = unsafe { TerminateProcess(handle, 1) };
    if terminated == 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            CloseHandle(handle);
        }
        return Err(error.to_string());
    }
    unsafe {
        WaitForSingleObject(handle, 2_000);
        CloseHandle(handle);
    }
    Ok(())
}

#[cfg(windows)]
fn restart_shell() -> Result<(), String> {
    std::thread::sleep(std::time::Duration::from_millis(800));
    if shell_is_running() {
        return Ok(());
    }
    std::process::Command::new("explorer.exe")
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn shell_is_running() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;

    ["Shell_TrayWnd", "Progman"].iter().any(|class| {
        let wide = wide_utf16(class);
        unsafe { !FindWindowW(wide.as_ptr(), std::ptr::null()).is_null() }
    })
}

#[cfg(windows)]
fn with_session<T>(
    paths: &[PathBuf],
    action: impl FnOnce(u32) -> Result<T, String>,
) -> Result<T, String> {
    use windows_sys::Win32::{
        Foundation::ERROR_SUCCESS,
        System::RestartManager::{
            RmEndSession, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
        },
    };

    let mut handle = 0_u32;
    let mut key = [0_u16; CCH_RM_SESSION_KEY as usize + 1];
    let status = unsafe { RmStartSession(&mut handle, 0, key.as_mut_ptr()) };
    if status != ERROR_SUCCESS {
        return Err(localized(
            format!("Не удалось запустить Restart Manager: {status}"),
            format!("Could not start Restart Manager: {status}"),
        ));
    }

    struct Session(u32);
    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                RmEndSession(self.0);
            }
        }
    }

    let session = Session(handle);
    let wide_paths: Vec<Vec<u16>> = paths.iter().map(|path| wide_path(path)).collect();
    let pointers: Vec<*const u16> = wide_paths.iter().map(|path| path.as_ptr()).collect();
    let status = unsafe {
        RmRegisterResources(
            session.0,
            pointers.len() as u32,
            pointers.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(localized(
            format!("Не удалось проверить занятость файлов: {status}"),
            format!("Could not check file locks: {status}"),
        ));
    }
    action(session.0)
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    crate::path_util::extended(path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn wide_utf16(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn wide_string(value: &[u16]) -> String {
    let len = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..len])
}

#[cfg(not(windows))]
pub fn find(_paths: &[PathBuf]) -> Result<LockScan, String> {
    Ok(LockScan::default())
}

#[cfg(not(windows))]
pub fn shutdown(_paths: &[PathBuf], _selected: &[u32]) -> Result<(), String> {
    Err(crate::i18n::tr("Закрытие занятых файлов поддерживается только в Windows").to_owned())
}

#[cfg(all(test, windows))]
mod tests {
    use super::{find, shutdown};
    use std::{
        fs,
        process::{Command, Stdio},
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn protects_shell_helpers_but_not_explorer() {
        assert!(super::is_protected_process("dwm.exe", ""));
        assert!(super::is_protected_process("", "winlogon.exe"));
        assert!(!super::is_protected_process(
            "explorer.exe",
            "Windows Explorer"
        ));
        assert!(!super::is_protected_process("notepad.exe", "Notepad"));
    }

    #[test]
    #[ignore = "spawns a helper process holding a file"]
    fn finds_and_closes_locked_folder() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("korobka_lock_{}_{}", std::process::id(), stamp));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("locked.txt");
        fs::write(&file, b"locked").unwrap();
        let escaped = file.display().to_string().replace('\'', "''");
        let script = format!(
            "$f=[IO.File]::Open('{escaped}','Open','Read','None'); Start-Sleep -Seconds 30"
        );
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let mut scan = None;
        for _ in 0..30 {
            thread::sleep(Duration::from_millis(150));
            let current = find(std::slice::from_ref(&dir)).unwrap();
            if current
                .processes
                .iter()
                .any(|process| process.pid == child.id())
            {
                scan = Some(current);
                break;
            }
        }
        let scan = scan.expect("Restart Manager did not report the locking process");
        assert!(
            scan.blocked_paths.iter().any(|path| path == &file),
            "the exact blocked file was not detected: {:?}",
            scan.blocked_paths
        );

        let direct = find(std::slice::from_ref(&file)).unwrap();
        assert!(
            direct
                .processes
                .iter()
                .any(|process| process.pid == child.id()),
            "Restart Manager did not report a directly registered file"
        );

        let selected = vec![child.id()];
        shutdown(std::slice::from_ref(&dir), &selected).unwrap();
        let mut exited = false;
        for _ in 0..30 {
            if child.try_wait().unwrap().is_some() {
                exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if !exited {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(exited, "the locking process did not close");
        let removed = fs::remove_dir_all(&dir);
        assert!(
            removed.is_ok(),
            "the folder is still locked after closing the process: {removed:?}"
        );
    }
}
