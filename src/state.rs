use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const DEMO: Uuid = Uuid::from_u128(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Mp4,
    Gif,
}

impl Format {
    pub fn detect(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("mp4") {
            Some(Self::Mp4)
        } else if extension.eq_ignore_ascii_case("gif") {
            Some(Self::Gif)
        } else {
            None
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Mp4 => "video/mp4",
            Self::Gif => "image/gif",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wallpaper {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    pub format: Format,
    pub bytes: u64,
}

impl Wallpaper {
    pub fn open(path: &Path) -> Result<Self, String> {
        let format = Format::detect(path).ok_or("Выберите файл MP4 или GIF.")?;
        let name = path
            .file_name()
            .ok_or("У файла нет имени.")?
            .to_string_lossy()
            .into_owned();
        let unreadable = || format!("Не удалось открыть «{name}».");
        let path = path.canonicalize().map_err(|_| unreadable())?;
        let info = File::open(&path)
            .and_then(|file| file.metadata())
            .map_err(|_| unreadable())?;
        if !info.is_file() || info.len() == 0 {
            return Err(format!("«{name}» пуст или не является файлом."));
        }
        Ok(Self {
            id: Uuid::new_v4(),
            name,
            path,
            format,
            bytes: info.len(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    #[default]
    Cover,
    Contain,
    Fill,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub fit: Fit,
    pub monitor: String,
    pub muted: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fit: Fit::Cover,
            monitor: "all".into(),
            muted: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
    pub id: Uuid,
    pub settings: Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiTheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub library: Vec<Wallpaper>,
    pub active: Option<Active>,
    pub draft: Settings,
    pub wallpapers_enabled: bool,
    pub startup_initialized: bool,
    pub pause_in_fullscreen: bool,
    pub theme: UiTheme,
    pub favorites: BTreeSet<Uuid>,
    pub auto_update: bool,
    pub last_update_check: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            library: Vec::new(),
            active: None,
            draft: Settings::default(),
            wallpapers_enabled: true,
            startup_initialized: false,
            pause_in_fullscreen: true,
            theme: UiTheme::default(),
            favorites: BTreeSet::new(),
            auto_update: true,
            last_update_check: 0,
        }
    }
}

impl Config {
    pub fn remove_wallpaper(&mut self, id: Uuid) -> Option<bool> {
        let index = self
            .library
            .iter()
            .position(|wallpaper| wallpaper.id == id)?;
        self.library.remove(index);
        self.favorites.remove(&id);
        let was_active = self.active.as_ref().is_some_and(|active| active.id == id);
        if was_active {
            self.active = None;
        }
        Some(was_active)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        serde_json::from_reader(file).map_err(io::Error::other)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        fs::create_dir_all(dir)?;
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer_pretty(&mut temp, self)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    pub fn format(&self, id: Uuid) -> Option<Format> {
        if id == DEMO {
            Some(Format::Mp4)
        } else {
            self.library
                .iter()
                .find(|wallpaper| wallpaper.id == id)
                .map(|wallpaper| wallpaper.format)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_existing_wallpapers_when_new_settings_are_missing() {
        let active = Active {
            id: DEMO,
            settings: Settings::default(),
        };
        let old_config = serde_json::json!({ "active": active });
        let config: Config = serde_json::from_value(old_config).unwrap();
        assert_eq!(config.active, Some(active));
        assert!(config.wallpapers_enabled);
        assert!(!config.startup_initialized);
        assert!(config.pause_in_fullscreen);
        assert_eq!(config.theme, UiTheme::System);
        assert!(config.favorites.is_empty());
        assert!(config.auto_update);
        assert_eq!(config.last_update_check, 0);
    }

    #[test]
    fn reopens_and_atomically_replaces_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(Config::load(&path).unwrap().active.is_none());

        let mut config = Config {
            active: Some(Active {
                id: DEMO,
                settings: Settings::default(),
            }),
            ..Config::default()
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap().active, config.active);

        config.wallpapers_enabled = false;
        config.startup_initialized = true;
        config.pause_in_fullscreen = false;
        config.theme = UiTheme::Light;
        config.favorites.insert(DEMO);
        config.save(&path).unwrap();
        let off = Config::load(&path).unwrap();
        assert_eq!(off.active, config.active);
        assert!(!off.wallpapers_enabled);
        assert!(off.startup_initialized);
        assert!(!off.pause_in_fullscreen);
        assert_eq!(off.theme, UiTheme::Light);
        assert!(off.favorites.contains(&DEMO));

        config.active = None;
        config.save(&path).unwrap();
        assert!(Config::load(&path).unwrap().active.is_none());
    }

    #[test]
    fn removing_active_wallpaper_clears_saved_selection_and_keeps_media() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("test.gif");
        fs::write(&source, b"GIF89a").unwrap();
        let wallpaper = Wallpaper::open(&source).unwrap();
        let id = wallpaper.id;
        let mut config = Config {
            library: vec![wallpaper],
            active: Some(Active {
                id,
                settings: Settings::default(),
            }),
            favorites: BTreeSet::from([id, DEMO]),
            ..Config::default()
        };
        assert_eq!(config.remove_wallpaper(id), Some(true));
        assert_eq!(config.remove_wallpaper(id), None);
        assert!(config.library.is_empty());
        assert!(config.active.is_none());
        assert_eq!(config.favorites, BTreeSet::from([DEMO]));
        assert_eq!(fs::read(source).unwrap(), b"GIF89a");
    }
}
