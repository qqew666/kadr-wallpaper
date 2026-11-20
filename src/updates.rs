use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ureq::{
    http::{Response, Uri, header::LOCATION},
    tls::{RootCerts, TlsConfig, TlsProvider},
};

#[cfg(windows)]
mod installer;
#[cfg(windows)]
pub use installer::{Mode, clean_installed, run_installer_if_requested};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = "qqew666/kadr-wallpaper";
const EXE: &str = "kadr-wallpaper.exe";
const MAX_EXE: u64 = 64 * 1024 * 1024;
const MAX_METADATA: u64 = 1024 * 1024;
const CHECK_INTERVAL: u64 = 12 * 60 * 60;
const STAGING_PREFIX: &str = ".kadr-update-";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    size: u64,
    digest: String,
}

pub struct Package {
    pub release: Release,
    candidate: File,
    target: PathBuf,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |age| age.as_secs())
}

pub fn check_due(last_check: u64, now: u64) -> bool {
    last_check == 0 || last_check > now || now - last_check >= CHECK_INTERVAL
}

fn client() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .max_redirects(0)
        .max_redirects_will_error(false)
        .timeout_global(Some(Duration::from_secs(45)))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: Uri) -> Result<Response<ureq::Body>, String> {
    agent
        .get(url)
        .header(
            "User-Agent",
            concat!("Kadr-Wallpaper/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(403 | 429) => {
                "GitHub временно ограничил запросы. Попробуйте позже.".into()
            }
            ureq::Error::StatusCode(code) => format!("GitHub ответил с кодом {code}."),
            _ => "Не удалось подключиться к GitHub. Проверьте интернет и попробуйте снова.".into(),
        })
}

pub fn check() -> Result<Option<Release>, String> {
    if !cfg!(target_arch = "x86_64") {
        return Err("Автообновление доступно для Windows x64.".into());
    }
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest")
        .parse()
        .expect("valid release URL");
    let response = get(&client(), url)?;
    if !response.status().is_success() {
        return Err("GitHub вернул неожиданный ответ.".into());
    }
    let mut data = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_METADATA + 1)
        .read_to_end(&mut data)
        .map_err(|error| format!("Не удалось прочитать ответ GitHub. {error}"))?;
    if data.len() as u64 > MAX_METADATA {
        return Err("Ответ GitHub слишком большой.".into());
    }
    let current = Version::parse(CURRENT).expect("valid Cargo package version");
    let release = serde_json::from_slice(&data).map_err(|_| "Ответ GitHub повреждён.")?;
    newer_release(release, &current)
}

fn newer_release(release: GithubRelease, current: &Version) -> Result<Option<Release>, String> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let version = release
        .tag_name
        .strip_prefix('v')
        .and_then(|tag| Version::parse(tag).ok())
        .ok_or("У релиза GitHub некорректная версия.")?;
    if version <= *current || !version.pre.is_empty() || !version.build.is_empty() {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == EXE && asset.state == "uploaded")
        .ok_or("В релизе GitHub пока нет EXE для Windows x64.")?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| valid_digest(digest))
        .ok_or("Для EXE не опубликована контрольная сумма SHA-256.")?;
    if !(1..=MAX_EXE).contains(&asset.size) {
        return Err("В релизе указан неверный размер EXE.".into());
    }
    Ok(Some(Release {
        version,
        size: asset.size,
        digest: digest.to_ascii_lowercase(),
    }))
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn allowed_download_url(url: &Uri) -> bool {
    url.scheme_str() == Some("https")
        && url.port().is_none()
        && matches!(
            url.host(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
                    | "github-releases.githubusercontent.com"
            )
        )
}

fn download_body(release: &Release) -> Result<ureq::Body, String> {
    let agent = client();
    let mut url: Uri = format!(
        "https://github.com/{REPOSITORY}/releases/download/v{}/{EXE}",
        release.version
    )
    .parse()
    .expect("valid asset URL");
    for _ in 0..4 {
        if !allowed_download_url(&url) {
            return Err("GitHub указал неизвестный адрес для загрузки.".into());
        }
        let response = get(&agent, url)?;
        if response.status().is_success() {
            return Ok(response.into_body());
        }
        if !response.status().is_redirection() {
            break;
        }
        url = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or("GitHub вернул неверный адрес загрузки.")?;
    }
    Err("Не удалось скачать EXE с GitHub.".into())
}

pub fn download(release: Release) -> Result<Package, String> {
    let target = std::env::current_exe()
        .map_err(|error| format!("Не удалось найти текущий EXE. {error}"))?;
    // Windows closes and removes this file even if the app exits during download.
    let mut candidate = tempfile::tempfile()
        .map_err(|error| format!("Не удалось открыть временный файл Windows. {error}"))?;
    let mut source = download_body(&release)?
        .into_reader()
        .take(release.size + 1);
    let size = io::copy(&mut source, &mut candidate)
        .map_err(|error| format!("Не удалось скачать EXE. {error}"))?;
    if size != release.size {
        return Err("Размер загруженного EXE не совпадает с релизом.".into());
    }
    candidate
        .flush()
        .map_err(|error| format!("Не удалось сохранить EXE. {error}"))?;
    verify_exe(&mut candidate, release.size, &release.digest)?;
    Ok(Package {
        release,
        candidate,
        target,
    })
}

fn verify_exe(file: &mut File, size: u64, digest: &str) -> Result<(), String> {
    fn verified(file: &mut File, size: u64, digest: &str) -> io::Result<bool> {
        if !(1..=MAX_EXE).contains(&size) || !valid_digest(digest) {
            return Ok(false);
        }
        if file.metadata()?.len() != size {
            return Ok(false);
        }
        file.rewind()?;
        let mut hasher = Sha256::new();
        let bytes = io::copy(&mut Read::by_ref(file).take(size + 1), &mut hasher)?;
        if bytes != size || !format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(digest) {
            return Ok(false);
        }
        file.seek(SeekFrom::Start(0))?;
        let mut dos = [0u8; 64];
        file.read_exact(&mut dos)?;
        let pe = u64::from(u32::from_le_bytes(dos[60..64].try_into().unwrap()));
        if &dos[..2] != b"MZ" || pe > size.saturating_sub(6) {
            return Ok(false);
        }
        file.seek(SeekFrom::Start(pe))?;
        let mut header = [0u8; 6];
        file.read_exact(&mut header)?;
        file.rewind()?;
        Ok(header == *b"PE\0\0\x64\x86")
    }

    match verified(file, size, digest) {
        Ok(true) => Ok(()),
        Ok(false) => {
            Err("Загруженный EXE не прошёл проверку SHA-256 или формата Windows x64.".into())
        }
        Err(error) => Err(format!("Не удалось проверить загруженный EXE. {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_new_stable_windows_releases_with_digest() {
        let sample = |tag: &str, digest: Option<String>| GithubRelease {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            assets: vec![GithubAsset {
                name: EXE.into(),
                state: "uploaded".into(),
                size: 100,
                digest,
            }],
        };
        let digest = Some(format!("sha256:{}", "a".repeat(64)));
        let current = Version::parse("0.6.9").unwrap();
        assert!(
            newer_release(sample("v0.6.9", digest.clone()), &current)
                .unwrap()
                .is_none()
        );
        assert!(
            newer_release(sample("v0.7.0-beta.1", digest.clone()), &current)
                .unwrap()
                .is_none()
        );
        assert!(newer_release(sample("v0.6.10", None), &current).is_err());
        assert!(
            newer_release(sample("v0.6.10", digest), &current)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn checks_digest_and_architecture_and_restricts_redirects() {
        let mut bytes = vec![0; 70];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60] = 64;
        bytes[64..].copy_from_slice(b"PE\0\0\x64\x86");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EXE);
        std::fs::write(&path, &bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let mut file = File::open(&path).unwrap();
        assert!(verify_exe(&mut file, bytes.len() as u64, &digest).is_ok());
        assert!(verify_exe(&mut file, bytes.len() as u64, &"0".repeat(64)).is_err());
        drop(file);
        bytes[68] = 0x4c;
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            verify_exe(
                &mut File::open(&path).unwrap(),
                bytes.len() as u64,
                &format!("{:x}", Sha256::digest(&bytes))
            )
            .is_err()
        );
        assert!(allowed_download_url(
            &"https://release-assets.githubusercontent.com/a"
                .parse()
                .unwrap()
        ));
        assert!(!allowed_download_url(
            &"http://github.com/a".parse().unwrap()
        ));
        assert!(!allowed_download_url(
            &"https://github.com.invalid/a".parse().unwrap()
        ));
        assert!(check_due(0, 900));
        assert!(!check_due(500, 900));
        assert!(check_due(901, 900));
    }
}
