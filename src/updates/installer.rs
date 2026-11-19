use super::{CANDIDATE, MAX_EXE, Package, STAGING_PREFIX, verify_exe};
use crate::startup;
use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    process::{self, Command},
    thread,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, ERROR_SHARING_VIOLATION, WAIT_OBJECT_0},
    Storage::FileSystem::SYNCHRONIZE,
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        WaitForSingleObject,
    },
};

const APPLY_ARG: &str = "--apply-kadr-update";
const HELPER: &str = "kadr-updater.exe";
const BACKUP: &str = "previous.exe";
const DONE: &str = "installed";

#[derive(Clone, Copy)]
pub enum Mode {
    Foreground,
    Background,
    Exit,
}

impl Mode {
    fn argument(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::Exit => "exit",
        }
    }

    fn restart(self, target: &Path) -> io::Result<()> {
        if matches!(self, Self::Exit) {
            return Ok(());
        }
        let mut child = Command::new(target);
        if matches!(self, Self::Background) {
            child.arg(startup::ARGUMENT);
        }
        child.spawn().map(|_| ())
    }
}

impl Package {
    pub fn launch(self, mode: Mode) -> Result<(), String> {
        let helper = self.stage.path().join(HELPER);
        let target = self.target.file_name().ok_or("У текущего EXE нет имени.")?;
        fs::copy(&self.target, &helper)
            .and_then(|_| {
                Command::new(&helper)
                    .arg(APPLY_ARG)
                    .arg(process::id().to_string())
                    .arg(target)
                    .arg(self.release.size.to_string())
                    .arg(&self.release.digest)
                    .arg(mode.argument())
                    .spawn()
            })
            .map_err(|error| format!("Не удалось запустить установку обновления. {error}"))?;
        let _ = self.stage.keep();
        Ok(())
    }
}

pub fn run_installer_if_requested() -> Option<Result<(), String>> {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(APPLY_ARG)) {
        return None;
    }
    Some(run_installer(args.collect()))
}

fn run_installer(args: Vec<OsString>) -> Result<(), String> {
    let [pid, target_name, size, digest, mode] = args.as_slice() else {
        return Err("Неверные параметры установки обновления.".into());
    };
    let bad_arguments = "Неверные параметры установки обновления.";
    let pid: u32 = pid
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|&id| id != 0 && id != process::id())
        .ok_or(bad_arguments)?;
    let size: u64 = size
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or(bad_arguments)?;
    let digest = digest.to_str().ok_or(bad_arguments)?;
    let mode = match mode.to_str() {
        Some("foreground") => Mode::Foreground,
        Some("background") => Mode::Background,
        Some("exit") => Mode::Exit,
        _ => return Err(bad_arguments.into()),
    };

    let helper =
        env::current_exe().map_err(|error| format!("Не удалось найти установщик. {error}"))?;
    let stage = helper
        .parent()
        .filter(|dir| is_stage(dir))
        .ok_or(bad_arguments)?;
    let target_name = Path::new(target_name);
    if helper.file_name() != Some(OsStr::new(HELPER))
        || target_name.file_name() != Some(target_name.as_os_str())
        || !target_name
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err(bad_arguments.into());
    }
    let target = stage.parent().ok_or(bad_arguments)?.join(target_name);

    wait_for_original(pid, &target)
        .map_err(|error| format!("Не удалось дождаться завершения Кадра. {error}"))?;
    let result = install(stage, &helper, &target, size, digest, mode);
    if result.is_err() && original_matches(&target, &helper).unwrap_or(false) {
        let _ = mode.restart(&target);
    }
    result
}

fn wait_for_original(pid: u32, target: &Path) -> io::Result<()> {
    let process = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut image = vec![0u16; 32_768];
    let mut size = image.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(process.as_raw_handle(), 0, image.as_mut_ptr(), &mut size)
    } == 0
    {
        if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
            return Ok(());
        }
        return Err(io::Error::last_os_error());
    }
    let image = PathBuf::from(OsString::from_wide(&image[..size as usize]));
    if image.canonicalize()? != target.canonicalize()? {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if unsafe { WaitForSingleObject(process.as_raw_handle(), 30_000) } != WAIT_OBJECT_0 {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

fn install(
    stage: &Path,
    helper: &Path,
    target: &Path,
    size: u64,
    digest: &str,
    mode: Mode,
) -> Result<(), String> {
    let candidate = stage.join(CANDIDATE);
    let backup = stage.join(BACKUP);
    verify_exe(&candidate, size, digest)?;

    let backup_original = || -> io::Result<()> {
        if !original_matches(target, helper)? {
            return Err(io::Error::other("Текущий EXE изменился во время загрузки."));
        }
        fs::copy(helper, &backup)?;
        fs::OpenOptions::new().write(true).open(&backup)?.sync_all()
    };
    backup_original()
        .map_err(|error| format!("Не удалось сохранить предыдущую версию. {error}"))?;

    replace(&candidate, target).map_err(|error| format!("Не удалось заменить EXE. {error}"))?;
    if let Err(error) = mode.restart(target) {
        if let Err(rollback) = replace(&backup, target) {
            return Err(format!(
                "Не удалось запустить новую версию и вернуть предыдущую. {error}\n{rollback}\n{}",
                backup.display()
            ));
        }
        return Err(format!(
            "Новая версия не запустилась. Предыдущий EXE восстановлен. {error}"
        ));
    }

    // Keep the old file until a subsequent successful launch of Kadr.
    let _ = fs::write(stage.join(DONE), b"");
    Ok(())
}

fn original_matches(target: &Path, helper: &Path) -> io::Result<bool> {
    let size = fs::metadata(target)?.len();
    Ok(size <= MAX_EXE
        && size == fs::metadata(helper)?.len()
        && fs::read(target)? == fs::read(helper)?)
}

fn replace(from: &Path, to: &Path) -> io::Result<()> {
    for _ in 0..6 {
        match fs::rename(from, to) {
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {
                thread::sleep(Duration::from_millis(200));
            }
            result => return result,
        }
    }
    fs::rename(from, to)
}

fn is_stage(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with(STAGING_PREFIX))
}

pub fn clean_installed() {
    let Ok(exe) = env::current_exe() else { return };
    let Some(parent) = exe.parent() else { return };
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for dir in entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
    {
        let path = dir.path();
        if is_stage(&path)
            && path.join(DONE).is_file()
            && remove_if_present(&path.join(HELPER)).is_ok()
            && remove_if_present(&path.join(BACKUP)).is_ok()
        {
            let _ = fs::remove_file(path.join(DONE));
            let _ = fs::remove_dir(path);
        }
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn replaces_verified_file_and_restores_previous_executable_if_launch_fails() {
        for mode in [Mode::Exit, Mode::Foreground] {
            let dir = tempfile::tempdir().unwrap();
            let stage = dir.path().join(".kadr-update-test");
            let target = dir.path().join("Kadr with spaces.exe");
            fs::create_dir(&stage).unwrap();
            let helper = stage.join(HELPER);
            fs::write(&target, b"previous executable").unwrap();
            fs::copy(&target, &helper).unwrap();

            // This fixture passes the header check but Windows cannot execute it.
            let mut bytes = vec![0; 70];
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60] = 64;
            bytes[64..].copy_from_slice(b"PE\0\0\x64\x86");
            let digest = format!("{:x}", Sha256::digest(&bytes));
            fs::write(stage.join(CANDIDATE), &bytes).unwrap();

            let result = install(&stage, &helper, &target, bytes.len() as u64, &digest, mode);
            if matches!(mode, Mode::Exit) {
                assert!(result.is_ok());
                assert_eq!(fs::read(&target).unwrap(), bytes);
                assert_eq!(
                    fs::read(stage.join(BACKUP)).unwrap(),
                    b"previous executable"
                );
            } else {
                assert!(result.unwrap_err().contains("Предыдущий EXE восстановлен"));
                assert_eq!(fs::read(&target).unwrap(), b"previous executable");
            }
        }
    }
}
