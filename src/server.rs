use crate::state::{Config, DEMO, Format, Wallpaper};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use uuid::Uuid;

const UI: &[u8] = include_bytes!("../ui/index.html");
const CSS: &[u8] = include_bytes!("../ui/app.css");
const APP: &[u8] = include_bytes!("../ui/app.js");
const PLAYER: &[u8] = include_bytes!("../ui/player.js");
const WALLPAPER: &[u8] = include_bytes!("../ui/wallpaper.html");
const WALLPAPER_JS: &[u8] = include_bytes!("../ui/wallpaper.js");
const AURORA: &[u8] = include_bytes!("../assets/aurora.mp4");
const POSTER: &[u8] = include_bytes!("../assets/aurora.webp");

pub struct MediaServer {
    pub base: String,
    library: Arc<RwLock<HashMap<Uuid, Wallpaper>>>,
    stop: Arc<AtomicBool>,
}

impl MediaServer {
    pub fn start(config: &Config) -> io::Result<Self> {
        let server = Arc::new(Server::http("127.0.0.1:0").map_err(io::Error::other)?);
        let authority = server
            .server_addr()
            .to_ip()
            .ok_or(io::ErrorKind::AddrNotAvailable)?
            .to_string();
        let prefix = format!("/{}/", Uuid::new_v4().simple());
        let this = Self {
            base: format!("http://{authority}{prefix}"),
            library: Arc::default(),
            stop: Arc::default(),
        };
        this.update(config);
        for worker in 0..8 {
            let server = server.clone();
            let prefix = prefix.clone();
            let authority = authority.clone();
            let library = this.library.clone();
            let stop = this.stop.clone();
            thread::Builder::new()
                .name(format!("media-{worker}"))
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match server.recv_timeout(Duration::from_millis(400)) {
                            Ok(Some(request)) => respond(request, &authority, &prefix, &library),
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                })?;
        }
        Ok(this)
    }

    pub fn update(&self, config: &Config) {
        if let Ok(mut library) = self.library.write() {
            *library = config
                .library
                .iter()
                .map(|wallpaper| (wallpaper.id, wallpaper.clone()))
                .collect();
        }
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid HTTP header")
}

fn respond(
    request: Request,
    authority: &str,
    prefix: &str,
    library: &RwLock<HashMap<Uuid, Wallpaper>>,
) {
    let valid_host = request
        .headers()
        .iter()
        .any(|value| value.field.equiv("Host") && value.value.as_str() == authority);
    if !valid_host {
        let _ = request.respond(Response::empty(403));
        return;
    }
    if !matches!(request.method(), Method::Get | Method::Head) {
        let _ = request.respond(Response::empty(405).with_header(header("Allow", "GET, HEAD")));
        return;
    }
    let Some(route) = request.url().strip_prefix(prefix) else {
        let _ = request.respond(Response::empty(404));
        return;
    };
    let asset: Option<(&[u8], &str)> = match route {
        "index.html" => Some((UI, "text/html; charset=utf-8")),
        "wallpaper.html" => Some((WALLPAPER, "text/html; charset=utf-8")),
        "app.css" => Some((CSS, "text/css; charset=utf-8")),
        "app.js" => Some((APP, "text/javascript; charset=utf-8")),
        "wallpaper.js" => Some((WALLPAPER_JS, "text/javascript; charset=utf-8")),
        "player.js" => Some((PLAYER, "text/javascript; charset=utf-8")),
        "aurora.webp" => Some((POSTER, "image/webp")),
        _ => None,
    };
    if let Some((body, content_type)) = asset {
        let _ = request.respond(Response::from_data(body).with_header(header("Content-Type", content_type))
            .with_header(header("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; media-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"))
            .with_header(header("X-Content-Type-Options", "nosniff"))
            .with_header(header("Referrer-Policy", "no-referrer")));
        return;
    }
    if let Some(id) = route
        .strip_prefix("media/")
        .and_then(|value| Uuid::parse_str(value).ok())
    {
        if id == DEMO {
            let _ = send_media(
                request,
                Cursor::new(AURORA),
                AURORA.len() as u64,
                Format::Mp4,
            );
            return;
        }
        let media = library
            .read()
            .ok()
            .and_then(|items| items.get(&id).cloned());
        if let Some(media) = media
            && let Ok(file) = File::open(&media.path)
            && let Ok(info) = file.metadata()
            && info.is_file()
        {
            let _ = send_media(request, file, info.len(), media.format);
            return;
        }
    }
    let _ = request.respond(Response::empty(404));
}

fn range(value: &str, length: u64) -> Option<(u64, u64)> {
    let (first, last) = value.strip_prefix("bytes=")?.split_once('-')?;
    let end_of_file = length.checked_sub(1)?;
    if first.is_empty() {
        let suffix = last.parse::<u64>().ok()?;
        return (suffix > 0).then(|| (length.saturating_sub(suffix), end_of_file));
    }
    let start = first.parse::<u64>().ok()?;
    let end = if last.is_empty() {
        end_of_file
    } else {
        last.parse::<u64>().ok()?.min(end_of_file)
    };
    (start <= end).then_some((start, end))
}

fn send_media(
    request: Request,
    mut data: impl Read + Seek,
    length: u64,
    format: Format,
) -> io::Result<()> {
    let requested = request
        .headers()
        .iter()
        .find(|value| value.field.equiv("Range"));
    let mut headers = vec![
        header("Content-Type", format.content_type()),
        header("Accept-Ranges", "bytes"),
        header("Cache-Control", "no-store"),
        header("X-Content-Type-Options", "nosniff"),
        header("Cross-Origin-Resource-Policy", "same-origin"),
    ];
    let (status, start, count) = if let Some(requested) = requested {
        let Some((start, end)) = range(requested.value.as_str(), length) else {
            return request.respond(
                Response::empty(416)
                    .with_header(header("Content-Range", &format!("bytes */{length}"))),
            );
        };
        headers.push(header(
            "Content-Range",
            &format!("bytes {start}-{end}/{length}"),
        ));
        (206, start, end - start + 1)
    } else {
        (200, 0, length)
    };

    data.seek(SeekFrom::Start(start))?;
    request.respond(Response::new(
        StatusCode(status),
        headers,
        data.take(count),
        usize::try_from(count).ok(),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpStream};

    #[test]
    fn serves_ranges_without_exposing_other_routes() {
        let server = MediaServer::start(&Config::default()).unwrap();
        let (authority, prefix) = server.base[7..].split_once('/').unwrap();
        let query = |path: &str| {
            let mut stream = TcpStream::connect(authority).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: {authority}\r\nRange: bytes=4-7\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response
        };
        let response = query(&format!("/{prefix}media/{DEMO}"));
        assert!(response.starts_with(b"HTTP/1.1 206"));
        assert!(response.ends_with(b"ftyp"));
        assert!(query(&format!("/media/{DEMO}")).starts_with(b"HTTP/1.1 404"));
        assert!(query(&format!("/{prefix}../Cargo.toml")).starts_with(b"HTTP/1.1 404"));
    }

    #[test]
    fn bounds_open_ended_and_suffix_ranges() {
        assert_eq!(range("bytes=3-", 10), Some((3, 9)));
        assert_eq!(range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(range("bytes=0-99", 10), Some((0, 9)));
        assert_eq!(range("bytes=10-", 10), None);
        assert_eq!(range("bytes=-0", 10), None);
        assert_eq!(range("bytes=0-1,4-5", 10), None);
        assert_eq!(range("bytes=0-", 0), None);
    }
}
