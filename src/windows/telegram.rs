use super::wide;
use anyhow::{Context, Result};
use std::{os::windows::process::CommandExt, path::PathBuf, process::Command};
use windows_sys::Win32::{
    Foundation::*,
    System::{Registry::*, Threading::*},
    UI::{Shell::CommandLineToArgvW, WindowsAndMessaging::*},
};

pub struct Launcher {
    executable: Option<PathBuf>,
    workdir: Option<PathBuf>,
    #[cfg(test)]
    recorded_opens: Option<std::rc::Rc<std::cell::Cell<usize>>>,
}

fn executable_from_command(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let path = if let Some(quoted) = command.strip_prefix('"') {
        quoted.split_once('"')?.0
    } else {
        let end = command.to_ascii_lowercase().find(".exe")? + 4;
        &command[..end]
    };
    let path = PathBuf::from(path);
    (path.is_absolute()
        && path
            .file_name()?
            .to_str()?
            .eq_ignore_ascii_case("Telegram.exe"))
    .then_some(path)
}

fn workdir_from_command(command: &str) -> Option<PathBuf> {
    let mut count = 0;
    // SAFETY: CommandLineToArgvW allocates count NUL-terminated strings, released by LocalFree.
    unsafe {
        let argv = CommandLineToArgvW(wide(command).as_ptr(), &mut count);
        if argv.is_null() {
            return None;
        }
        let args: Vec<String> = std::slice::from_raw_parts(argv, count as usize)
            .iter()
            .map(|pointer| {
                let mut length = 0;
                while *pointer.add(length) != 0 {
                    length += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(*pointer, length))
            })
            .collect();
        LocalFree(argv.cast());
        args.windows(2)
            .find(|pair| pair[0] == "-workdir")
            .map(|pair| PathBuf::from(&pair[1]))
            .filter(|path| path.is_absolute())
    }
}

fn registered_executable() -> Option<(PathBuf, Option<PathBuf>)> {
    for (root, key) in [
        (
            HKEY_CURRENT_USER,
            "Software\\Classes\\tg\\shell\\open\\command",
        ),
        (HKEY_CLASSES_ROOT, "tg\\shell\\open\\command"),
        (
            HKEY_CURRENT_USER,
            "Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\Telegram.exe",
        ),
        (
            HKEY_LOCAL_MACHINE,
            "Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\Telegram.exe",
        ),
    ] {
        let mut data = [0u16; 2048];
        let mut bytes = (data.len() * 2) as u32;
        // SAFETY: registry handles are predefined, writable buffer capacity matches the byte count.
        let result = unsafe {
            RegGetValueW(
                root,
                wide(key).as_ptr(),
                std::ptr::null(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if result == ERROR_SUCCESS {
            let length = data.iter().position(|v| *v == 0).unwrap_or(data.len());
            let command = String::from_utf16_lossy(&data[..length]);
            if let Some(path) = executable_from_command(&command)
                && path.is_file()
            {
                return Some((path, workdir_from_command(&command)));
            }
        }
    }
    None
}

impl Launcher {
    pub fn new() -> Self {
        let target = registered_executable().or_else(|| {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .map(|p| p.join("Telegram Desktop/Telegram.exe"))
                .filter(|p| p.is_file())
                .map(|path| (path, None))
        });
        let (executable, workdir) = target
            .map(|(path, workdir)| (Some(path), workdir))
            .unwrap_or_default();
        Self {
            executable,
            workdir,
            #[cfg(test)]
            recorded_opens: None,
        }
    }

    pub fn open(&mut self) -> Result<()> {
        #[cfg(test)]
        if let Some(counter) = &self.recorded_opens {
            counter.set(counter.get() + 1);
            return Ok(());
        }
        let mut found: HWND = std::ptr::null_mut();
        // SAFETY: callback writes only to the stack pointer supplied for the synchronous enumeration.
        unsafe {
            EnumWindows(Some(find_telegram), (&mut found as *mut HWND) as isize);
        }
        if !found.is_null() {
            // SAFETY: EnumWindows returned an existing top-level Telegram window.
            unsafe {
                ShowWindowAsync(
                    found,
                    if IsIconic(found) != 0 {
                        SW_RESTORE
                    } else {
                        SW_SHOW
                    },
                );
                if SetForegroundWindow(found) != 0 {
                    return Ok(());
                }
            }
        }
        if self.executable.as_ref().is_some_and(|path| !path.is_file()) {
            self.executable = None;
        }
        if self.executable.is_none() {
            let target = registered_executable();
            (self.executable, self.workdir) = target
                .map(|(path, workdir)| (Some(path), workdir))
                .unwrap_or_default();
        }
        if let Some(executable) = &self.executable {
            let mut command = Command::new(executable);
            if let Some(workdir) = &self.workdir {
                command.arg("-workdir").arg(workdir);
            }
            command
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .context("Не удалось запустить Telegram")?;
            return Ok(());
        }
        super::open("tg://").context(
            "Telegram не найден. Установи Telegram Desktop или зарегистрируй обработчик tg://",
        )
    }

    #[cfg(test)]
    pub fn recording(counter: std::rc::Rc<std::cell::Cell<usize>>) -> Self {
        Self {
            executable: None,
            workdir: None,
            recorded_opens: Some(counter),
        }
    }
}

unsafe extern "system" fn find_telegram(hwnd: HWND, context: LPARAM) -> i32 {
    // SAFETY: callback is called by EnumWindows; handles are checked and process resources are closed.
    unsafe {
        if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
            return 1;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return 1;
        }
        let mut path = [0u16; 1024];
        let mut length = path.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut length);
        CloseHandle(process);
        if ok != 0
            && PathBuf::from(String::from_utf16_lossy(&path[..length as usize]))
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("Telegram.exe"))
        {
            *(context as *mut HWND) = hwnd;
            return 0;
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_quoted_and_legacy_associations_without_running_registry_arguments() {
        let expected = Some(PathBuf::from(
            r"C:\Program Files\Telegram Desktop\Telegram.exe",
        ));
        assert_eq!(
            executable_from_command(
                r#""C:\Program Files\Telegram Desktop\Telegram.exe" -workdir "C:\data" -- "%1""#
            ),
            expected
        );
        assert_eq!(
            executable_from_command(r"C:\Program Files\Telegram Desktop\Telegram.exe -- %1"),
            expected
        );
        assert_eq!(
            executable_from_command(r#""C:\Windows\cmd.exe" /c Telegram.exe"#),
            None
        );
        assert_eq!(executable_from_command("Telegram.exe %1"), None);
        assert_eq!(
            workdir_from_command(r#""C:\Telegram.exe" -workdir "D:\Telegram data" -- "%1""#),
            Some(PathBuf::from(r"D:\Telegram data"))
        );
        assert_eq!(
            workdir_from_command(r#""C:\Telegram.exe" -workdir relative -- "%1""#),
            None
        );
    }
}
