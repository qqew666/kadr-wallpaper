use super::{MAX_EXE, Package, STAGING_PREFIX, verify_exe};
use crate::startup;
use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Seek},
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

const APPLY_ARG: &str = "--apply-kadr-update-file";
const CLEAN_ARG: &str = "--clean-kadr-update";
const HELPER_PREFIX: &str = "kadr-update-";
const PAYLOAD_EXTENSION: &str = "download";
const TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

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

    fn restart(self, target: &Path, helper: &Path) -> io::Result<()> {
        if matches!(self, Self::Exit) {
            return Ok(());
        }
        let mut child = Command::new(target);
        if matches!(self, Self::Background) {
            child.arg(startup::ARGUMENT);
        }
        if let Some(name) = helper.file_name() {
            child.arg(CLEAN_ARG).arg(name);
        }
        child.spawn().map(|_| ())
    }
}

impl Package {
    pub fn launch(&mut self, mode: Mode) -> Result<(), String> {
        let parent = self
            .target
            .parent()
            .ok_or("Не удалось найти папку Кадра.")?;
        tempfile::NamedTempFile::new_in(parent).map_err(|error| {
            format!("Windows запрещает замену EXE в этой папке. Установите Кадр через Setup.exe. {error}")
        })?;

        let mut start = || -> io::Result<()> {
            let helper = tempfile::Builder::new()
                .prefix(HELPER_PREFIX)
                .rand_bytes(12)
                .suffix(".exe")
                .tempfile()?
                .into_temp_path();
            fs::copy(&self.target, &helper)?;
            let payload = helper.with_extension(PAYLOAD_EXTENSION);
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&payload)?;
            let result = (|| {
                self.candidate.rewind()?;
                io::copy(&mut self.candidate, &mut output)?;
                output.sync_all()?;
                drop(output);
                let helper = helper.keep()?;
                let result = Command::new(&helper)
                    .arg(APPLY_ARG)
                    .arg(process::id().to_string())
                    .arg(&self.target)
                    .arg(self.release.size.to_string())
                    .arg(&self.release.digest)
                    .arg(mode.argument())
                    .spawn();
                if result.is_err() {
                    let _ = fs::remove_file(&helper);
                }
                result.map(|_| ())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&payload);
            }
            result
        };
        start().map_err(|error| format!("Не удалось запустить установку обновления. {error}"))
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
    let [pid, target, size, digest, mode] = args.as_slice() else {
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
    let target = Path::new(target);
    if !helper
        .file_name()
        .is_some_and(|name| update_name(name, "exe"))
        || helper
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            != env::temp_dir().canonicalize().ok()
        || !target.is_absolute()
        || !target
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        || target.canonicalize().ok() == helper.canonicalize().ok()
    {
        return Err(bad_arguments.into());
    }

    let payload = helper.with_extension(PAYLOAD_EXTENSION);
    let result = wait_for_original(pid, target)
        .map_err(|error| format!("Не удалось дождаться завершения Кадра. {error}"))
        .and_then(|_| install(&payload, &helper, target, size, digest, mode));
    let _ = fs::remove_file(payload);
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
    payload: &Path,
    helper: &Path,
    target: &Path,
    size: u64,
    digest: &str,
    mode: Mode,
) -> Result<(), String> {
    let original = fs::metadata(target).and_then(|target_info| {
        Ok(target_info.len() <= MAX_EXE
            && target_info.len() == fs::metadata(helper)?.len()
            && fs::read(target)? == fs::read(helper)?)
    });
    if !original.unwrap_or(false) {
        return Err("Текущий EXE изменился во время загрузки. Запустите Кадр заново.".into());
    }
    let apply = || -> Result<(), String> {
        let mut candidate = File::open(payload)
            .map_err(|error| format!("Не удалось открыть обновление. {error}"))?;
        verify_exe(&mut candidate, size, digest)?;
        replace_from(&mut candidate, target)
            .map_err(|error| format!("Не удалось заменить EXE. {error}"))
    };
    if let Err(error) = apply() {
        let _ = mode.restart(target, helper);
        return Err(error);
    }
    if let Err(error) = mode.restart(target, helper) {
        if let Err(rollback) =
            File::open(helper).and_then(|mut file| replace_from(&mut file, target))
        {
            return Err(format!(
                "Не удалось запустить новую версию и вернуть предыдущую. {error}\n{rollback}\n{}",
                helper.display()
            ));
        }
        let _ = mode.restart(target, helper);
        return Err(format!(
            "Новая версия не запустилась. Предыдущий EXE восстановлен. {error}"
        ));
    }
    Ok(())
}

fn replace_from(source: &mut File, target: &Path) -> io::Result<()> {
    // Replace atomically on the target volume even when %TEMP% is elsewhere.
    let parent = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut replacement = tempfile::NamedTempFile::new_in(parent)?;
    io::copy(source, &mut replacement)?;
    replacement.as_file().sync_all()?;
    let mut path = replacement.into_temp_path();
    for _ in 0..6 {
        match path.persist(target) {
            Err(error) if error.error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {
                path = error.path;
                thread::sleep(Duration::from_millis(200));
            }
            result => return result.map_err(|error| error.error),
        }
    }
    path.persist(target).map_err(|error| error.error)
}

fn update_name(name: &OsStr, extension: &str) -> bool {
    let path = Path::new(name);
    path.file_name() == Some(name)
        && path.extension() == Some(OsStr::new(extension))
        && path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|stem| {
                stem.strip_prefix(HELPER_PREFIX).is_some_and(|id| {
                    id.len() == 12 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
            })
}

pub fn clean_installed() {
    let temp = env::temp_dir();
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == CLEAN_ARG {
            if let Some(name) = args.next().filter(|name| update_name(name, "exe")) {
                let helper = temp.join(name);
                for _ in 0..20 {
                    if remove_if_present(&helper).is_ok() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(250));
                }
            }
            break;
        }
    }
    clean_stale_files(&temp);
    clean_legacy_stages();
}

fn clean_stale_files(temp: &Path) {
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if (update_name(&name, "exe") || update_name(&name, PAYLOAD_EXTENSION))
            && entry.metadata().is_ok_and(|info| {
                info.is_file()
                    && info
                        .modified()
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age >= TEMP_MAX_AGE)
            })
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn clean_legacy_stages() {
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
        if dir
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGING_PREFIX))
            && path.join("installed").is_file()
            && remove_if_present(&path.join("kadr-updater.exe")).is_ok()
            && remove_if_present(&path.join("previous.exe")).is_ok()
        {
            let _ = fs::remove_file(path.join("installed"));
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
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Kadr with spaces.exe");
        let helper = dir.path().join("helper.exe");
        let payload = dir.path().join("candidate.download");
        fs::write(&helper, b"previous executable").unwrap();

        // This fixture passes the header check but Windows cannot execute it.
        let mut bytes = vec![0; 70];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60] = 64;
        bytes[64..].copy_from_slice(b"PE\0\0\x64\x86");
        let digest = format!("{:x}", Sha256::digest(&bytes));
        fs::write(&payload, &bytes).unwrap();

        fs::copy(&helper, &target).unwrap();
        let result = install(
            &payload,
            &helper,
            &target,
            bytes.len() as u64,
            &digest,
            Mode::Exit,
        );
        assert!(result.is_ok());
        assert_eq!(fs::read(&target).unwrap(), bytes);

        fs::copy(&helper, &target).unwrap();
        let result = install(
            &payload,
            &helper,
            &target,
            bytes.len() as u64,
            &digest,
            Mode::Foreground,
        );
        assert!(result.unwrap_err().contains("Предыдущий EXE восстановлен"));
        assert_eq!(fs::read(&target).unwrap(), b"previous executable");

        fs::write(&target, b"replaced externally").unwrap();
        assert!(
            install(
                &payload,
                &helper,
                &target,
                bytes.len() as u64,
                &digest,
                Mode::Exit
            )
            .is_err()
        );
        assert_eq!(fs::read(target).unwrap(), b"replaced externally");
    }
}
