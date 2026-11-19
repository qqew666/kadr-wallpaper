use std::{env, ffi::OsString, io, os::windows::ffi::OsStrExt, path::Path};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE},
};

pub const ARGUMENT: &str = "--startup";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const ENTRY: &str = "KadrWallpaper";

pub fn is_enabled() -> io::Result<bool> {
    match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_QUERY_VALUE) {
        Ok(key) => Ok(read_command(&key)?.is_some()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn set_enabled(enabled: bool) -> io::Result<()> {
    let user = RegKey::predef(HKEY_CURRENT_USER);
    if enabled {
        let command = startup_command(&env::current_exe()?);
        // Windows Run entries accept at most 260 characters.
        if command.encode_wide().count() > 260 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Путь к EXE слишком длинный для автозапуска Windows.",
            ));
        }
        let (key, _) = user.create_subkey_with_flags(RUN_KEY, KEY_QUERY_VALUE | KEY_SET_VALUE)?;
        if read_command(&key)?.as_ref() != Some(&command) {
            key.set_value(ENTRY, &command)?;
        }
        return Ok(());
    }

    match user
        .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
        .and_then(|key| key.delete_value(ENTRY))
    {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_command(key: &RegKey) -> io::Result<Option<OsString>> {
    match key.get_value(ENTRY) {
        Ok(command) => Ok(Some(command)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn startup_command(exe: &Path) -> OsString {
    let mut command = OsString::from("\"");
    command.push(exe.as_os_str());
    command.push("\" ");
    command.push(ARGUMENT);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_executable_paths_with_spaces() {
        assert_eq!(
            startup_command(Path::new(r"C:\Мои программы\kadr-wallpaper.exe")),
            OsString::from(r#""C:\Мои программы\kadr-wallpaper.exe" --startup"#),
        );
    }
}
