use crate::{
    desktop::{self, BackdropWindow, Desktop, Display},
    server::MediaServer,
    state::{Active, Config, DEMO, Format, Settings, Wallpaper},
};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::PathBuf,
    ptr::null_mut,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    window::{Icon as WindowIcon, Window, WindowBuilder},
};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
    System::Threading::CreateMutexW,
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
};
use wry::{
    DragDropEvent, NewWindowResponse, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};

const ICON: &[u8] = include_bytes!("../assets/tray.rgba");
const CHECK_INTERVAL: Duration = Duration::from_secs(2);
const RETRY_INTERVAL: Duration = Duration::from_secs(25);
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Command {
    Ready,
    Import,
    Apply { id: Uuid, settings: Settings },
    Remove { id: Uuid },
    Pause,
    Stop,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum PlayerEvent {
    Ready,
    Error { message: String },
}

enum AppEvent {
    Command(Command),
    Player(u64, usize, PlayerEvent),
    Import(Vec<PathBuf>),
    Drag(bool),
    Show,
    Exit,
}

#[derive(Serialize)]
struct Notice {
    message: String,
    error: bool,
}

#[derive(Serialize)]
struct Item<'a> {
    id: Uuid,
    name: &'a str,
    format: Format,
    bytes: u64,
}

struct Backdrop {
    web: WebView,
    window: BackdropWindow,
    ready: bool,
}

struct Scene {
    generation: u64,
    displays: Vec<Display>,
    layers: Vec<Backdrop>,
}

impl Scene {
    fn healthy(&self) -> bool {
        self.layers.iter().all(|layer| layer.window.healthy())
    }

    fn play(&self, playing: bool) {
        for layer in &self.layers {
            if playing {
                layer.window.show(true);
                let _ = layer.web.evaluate_script("window.kadrPlayback?.(true)");
            } else {
                let _ = layer.web.evaluate_script("window.kadrPlayback?.(false)");
                layer.window.show(false);
            }
        }
    }
}

struct Pending {
    active: Active,
    scene: Scene,
    save: bool,
    deadline: Instant,
}

struct Manager {
    // Drop WebView controllers before disposing of their native host windows.
    pending: Option<Pending>,
    scene: Option<Scene>,
    web: WebView,
    window: Window,
    _tray: TrayIcon,
    tray_pause: MenuItem,
    tray_stop: MenuItem,
    proxy: EventLoopProxy<AppEvent>,
    server: MediaServer,
    config: Config,
    config_path: PathBuf,
    displays: Vec<Display>,
    paused: bool,
    ready: bool,
    notice: Option<Notice>,
    select: Option<Uuid>,
    retry_at: Instant,
    generation: u64,
}

pub fn show_error(message: &str) {
    let wide: Vec<_> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            null_mut(),
            wide.as_ptr(),
            windows_sys::w!("Кадр — живые обои"),
            MB_OK | MB_ICONERROR,
        );
    }
}

struct Instance(HANDLE);

impl Instance {
    fn acquire() -> Result<Self, String> {
        unsafe {
            let handle = CreateMutexW(null_mut(), 0, windows_sys::w!("Local\\KadrWallpaper"));
            let error = GetLastError();
            if handle.is_null() {
                return Err(format!("Не удалось запустить Кадр. Код Windows {error}."));
            }
            let instance = Self(handle);
            if error == ERROR_ALREADY_EXISTS {
                return Err(
                    "Кадр уже работает. Откройте его через значок в области уведомлений.".into(),
                );
            }
            Ok(instance)
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub fn run() -> Result<(), String> {
    let _instance = Instance::acquire()?;
    let data_dir = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("Переменная LOCALAPPDATA недоступна.")?
        .join("KadrWallpaper");
    fs::create_dir_all(&data_dir).map_err(|error| format!("Папка настроек недоступна. {error}"))?;
    let config_path = data_dir.join("settings.json");
    let config = Config::load(&config_path).map_err(|error| {
        format!(
            "Не удалось прочитать настройки. {error}\n{}",
            config_path.display()
        )
    })?;
    let server = MediaServer::start(&config)
        .map_err(|error| format!("Не удалось запустить локальное воспроизведение. {error}"))?;
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("Кадр — живые обои")
        .with_inner_size(LogicalSize::new(1160, 760))
        .with_min_inner_size(LogicalSize::new(660, 510))
        .with_window_icon(WindowIcon::from_rgba(ICON.to_vec(), 32, 32).ok())
        .with_visible(false)
        .build(&event_loop)
        .map_err(|error| format!("Не удалось создать окно. {error}"))?;

    let mut context = WebContext::new(Some(data_dir.join("WebView2")));
    let ui_url = format!("{}index.html", server.base);
    let commands = proxy.clone();
    let drag = proxy.clone();
    let web = secure_view(WebViewBuilder::new_with_web_context(&mut context), &ui_url)
        .with_ipc_handler(move |request| {
            if let Ok(command) = serde_json::from_str::<Command>(request.body()) {
                let _ = commands.send_event(AppEvent::Command(command));
            }
        })
        .with_drag_drop_handler(move |event| {
            match event {
                DragDropEvent::Enter { paths, .. } if !paths.is_empty() => {
                    let _ = drag.send_event(AppEvent::Drag(true));
                }
                DragDropEvent::Drop { paths, .. } => {
                    let _ = drag.send_event(AppEvent::Drag(false));
                    let _ = drag.send_event(AppEvent::Import(paths));
                }
                DragDropEvent::Leave => {
                    let _ = drag.send_event(AppEvent::Drag(false));
                }
                _ => {}
            }
            true
        })
        .build(&window)
        .map_err(|error| {
            format!("Не удалось запустить Microsoft Edge WebView2. Установите или обновите WebView2 Runtime и попробуйте снова.\n{error}")
        })?;

    let tray_menu = Menu::new();
    let open = MenuItem::new("Открыть Кадр", true, None);
    let tray_pause = MenuItem::new("Пауза", false, None);
    let tray_stop = MenuItem::new("Снять обои", false, None);
    let exit = MenuItem::new("Выход", true, None);
    tray_menu
        .append_items(&[
            &open,
            &tray_pause,
            &tray_stop,
            &PredefinedMenuItem::separator(),
            &exit,
        ])
        .map_err(|error| format!("Не удалось создать меню. {error}"))?;
    let (open_id, pause_id, stop_id, exit_id) = (
        open.id().clone(),
        tray_pause.id().clone(),
        tray_stop.id().clone(),
        exit.id().clone(),
    );
    let menu_events = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = if event.id == open_id {
            AppEvent::Show
        } else if event.id == pause_id {
            AppEvent::Command(Command::Pause)
        } else if event.id == stop_id {
            AppEvent::Command(Command::Stop)
        } else if event.id == exit_id {
            AppEvent::Exit
        } else {
            return;
        };
        let _ = menu_events.send_event(action);
    }));
    let tray_events = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            let _ = tray_events.send_event(AppEvent::Show);
        }
    }));

    let icon = Icon::from_rgba(ICON.to_vec(), 32, 32)
        .map_err(|error| format!("Не удалось загрузить значок. {error}"))?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_menu_on_left_click(false)
        .with_icon(icon)
        .with_tooltip("Кадр — живые обои")
        .build()
        .map_err(|error| format!("Не удалось добавить значок в область уведомлений. {error}"))?;

    let now = Instant::now();
    let mut manager = Manager {
        web,
        window,
        _tray: tray,
        tray_pause,
        tray_stop,
        proxy,
        server,
        config,
        config_path,
        displays: desktop::displays(&event_loop),
        pending: None,
        scene: None,
        paused: false,
        ready: false,
        notice: None,
        select: None,
        retry_at: now,
        generation: 0,
    };
    let mut check_at = now;
    manager.window.set_visible(true);

    event_loop.run(move |event, target, control| {
        if Instant::now() >= check_at {
            manager.check(target);
            check_at = Instant::now() + CHECK_INTERVAL;
        }
        *control = ControlFlow::WaitUntil(check_at);
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } if window_id == manager.window.id() => {
                let _ = manager.web.evaluate_script("window.kadr?.visible(false)");
                manager.window.set_visible(false);
            }
            Event::UserEvent(event) => match event {
                AppEvent::Command(command) => manager.command(command),
                AppEvent::Player(generation, index, event) => {
                    manager.handle_player(generation, index, event)
                }
                AppEvent::Import(paths) => manager.import(paths),
                AppEvent::Drag(drag) => {
                    let _ = manager
                        .web
                        .evaluate_script(&format!("window.kadr?.drag({drag})"));
                }
                AppEvent::Show => {
                    manager.window.set_visible(true);
                    manager.window.set_minimized(false);
                    manager.window.set_focus();
                    let _ = manager.web.evaluate_script("window.kadr?.visible(true)");
                }
                AppEvent::Exit => {
                    manager.pending = None;
                    manager.scene = None;
                    *control = ControlFlow::Exit;
                }
            },
            _ => {}
        }
    });
}

fn secure_view<'a>(builder: WebViewBuilder<'a>, url: &str) -> WebViewBuilder<'a> {
    let allowed = url.to_owned();
    builder
        .with_url(url)
        .with_navigation_handler(move |target| target == allowed)
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_default_context_menus(false)
        .with_browser_accelerator_keys(false)
        .with_devtools(false)
        .with_autoplay(true)
        .with_background_color((16, 16, 21, 255))
}

impl Manager {
    fn status(&self) -> &'static str {
        if self.pending.is_some() {
            "applying"
        } else if self.paused {
            "paused"
        } else if self.scene.is_some() {
            "running"
        } else if self.config.active.is_some() {
            "waiting"
        } else {
            "idle"
        }
    }

    fn render(&mut self) {
        self.tray_pause
            .set_enabled(self.scene.is_some() && self.pending.is_none());
        self.tray_pause.set_text(if self.paused {
            "Продолжить"
        } else {
            "Пауза"
        });
        self.tray_stop
            .set_enabled(self.config.active.is_some() && self.pending.is_none());
        if !self.ready {
            return;
        }
        let mut items = vec![Item {
            id: DEMO,
            name: "Северное сияние",
            format: Format::Mp4,
            bytes: 0,
        }];
        items.extend(self.config.library.iter().map(|wallpaper| Item {
            id: wallpaper.id,
            name: &wallpaper.name,
            format: wallpaper.format,
            bytes: wallpaper.bytes,
        }));
        let state = serde_json::json!({
            "items": items,
            "monitors": self.displays,
            "active": self.config.active,
            "draft": self.config.draft,
            "status": self.status(),
            "notice": self.notice,
            "select": self.select,
        });
        if self
            .web
            .evaluate_script(&format!("window.kadr?.render({state})"))
            .is_ok()
        {
            self.notice = None;
            self.select = None;
        }
    }

    fn notice(&mut self, message: impl Into<String>, error: bool) {
        self.notice = Some(Notice {
            message: message.into(),
            error,
        });
        self.render();
    }

    fn save(&mut self, config: Config) -> bool {
        match config.save(&self.config_path) {
            Ok(()) => {
                self.config = config;
                self.server.update(&self.config);
                true
            }
            Err(error) => {
                self.notice(format!("Не удалось сохранить настройки. {error}"), true);
                false
            }
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Ready => {
                self.ready = true;
                self.render();
            }
            Command::Import => {
                if let Some(paths) = rfd::FileDialog::new()
                    .set_title("Импортировать живые обои")
                    .add_filter("Видеофайлы и анимации", &["mp4", "gif"])
                    .set_parent(&self.window)
                    .pick_files()
                {
                    self.import(paths);
                }
            }
            Command::Apply { id, settings } if self.pending.is_none() => {
                if self.begin(Active { id, settings }, true).is_ok() {
                    self.render();
                }
            }
            Command::Remove { id } if id != DEMO && self.pending.is_none() => {
                let mut config = self.config.clone();
                config.library.retain(|item| item.id != id);
                let was_active = config.active.as_ref().is_some_and(|active| active.id == id);
                if was_active {
                    config.active = None;
                }
                if self.save(config) {
                    if was_active {
                        self.scene = None;
                        self.paused = false;
                    }
                    self.notice("Файл убран из библиотеки.", false);
                }
            }
            Command::Pause if self.pending.is_none() => {
                if let Some(scene) = &self.scene {
                    self.paused = !self.paused;
                    scene.play(!self.paused);
                    self.render();
                }
            }
            Command::Stop if self.pending.is_none() => {
                let mut config = self.config.clone();
                config.active = None;
                if self.save(config) {
                    self.scene = None;
                    self.paused = false;
                    self.render();
                }
            }
            _ => {}
        }
    }

    fn import(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let mut config = self.config.clone();
        let mut selected = None;
        let mut errors = Vec::new();
        let initial = config.library.len();
        for path in paths {
            match Wallpaper::open(&path) {
                Ok(wallpaper) => {
                    let id = if let Some(old) = config
                        .library
                        .iter()
                        .find(|item| item.path == wallpaper.path)
                    {
                        old.id
                    } else {
                        let id = wallpaper.id;
                        config.library.push(wallpaper);
                        id
                    };
                    selected.get_or_insert(id);
                }
                Err(error) => errors.push(error),
            }
        }
        let added = config.library.len() - initial;
        if added > 0 && !self.save(config) {
            return;
        }
        self.select = selected;
        if let Some(error) = errors.first() {
            let message = if added == 0 {
                error.to_string()
            } else {
                format!("Добавлено файлов: {added}. {error}")
            };
            self.notice(message, true);
        } else {
            let message = if added == 0 {
                "Файл уже в библиотеке.".into()
            } else {
                format!("Добавлено файлов: {added}.")
            };
            self.notice(message, false);
        }
    }

    fn begin(&mut self, active: Active, save: bool) -> Result<(), ()> {
        let result = self.load_scene(&active);
        match result {
            Ok(scene) => {
                self.pending = Some(Pending {
                    active,
                    scene,
                    save,
                    deadline: Instant::now() + LOAD_TIMEOUT,
                });
                Ok(())
            }
            Err(message) => {
                self.retry_at = Instant::now() + RETRY_INTERVAL;
                self.notice(message, true);
                Err(())
            }
        }
    }

    fn load_scene(&mut self, active: &Active) -> Result<Scene, String> {
        let format = self
            .config
            .format(active.id)
            .ok_or("Обои отсутствуют в библиотеке.")?;
        let displays: Vec<_> = desktop::targets(&self.displays, &active.settings.monitor)
            .into_iter()
            .cloned()
            .collect();
        if displays.is_empty() {
            return Err("Выбранный монитор отключён.".into());
        }

        let desktop = Desktop::find().map_err(|_| "Explorer не открыл слой рабочего стола. Попробуйте перезапустить Проводник Windows.")?;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let url = format!("{}wallpaper.html", self.server.base);
        let mut layers = Vec::with_capacity(displays.len());
        for (index, display) in displays.iter().enumerate() {
            let window = desktop.create(display).map_err(|error| {
                format!(
                    "Не удалось подключиться к экрану «{}». {error}",
                    display.label
                )
            })?;
            let options = serde_json::json!({
                "id": active.id,
                "format": format,
                "fit": active.settings.fit,
                "muted": active.settings.muted || index != 0,
            });
            let proxy = self.proxy.clone();
            let web = secure_view(WebViewBuilder::new(), &url)
                .with_environment(self.web.environment())
                .with_focused(false)
                .with_initialization_script(format!("window.kadrWallpaper = {options}"))
                .with_ipc_handler(move |request| {
                    if let Ok(event) = serde_json::from_str::<PlayerEvent>(request.body()) {
                        let _ = proxy.send_event(AppEvent::Player(generation, index, event));
                    }
                })
                .build(&window)
                .map_err(|error| {
                    format!(
                        "Не удалось открыть обои на экране «{}». {error}",
                        display.label
                    )
                })?;
            layers.push(Backdrop {
                web,
                window,
                ready: false,
            });
        }
        Ok(Scene {
            generation,
            layers,
            displays,
        })
    }

    fn handle_player(&mut self, generation: u64, index: usize, event: PlayerEvent) {
        if let Some(pending) = &mut self.pending
            && pending.scene.generation == generation
        {
            match event {
                PlayerEvent::Ready => {
                    if let Some(layer) = pending.scene.layers.get_mut(index) {
                        layer.ready = true;
                    }
                    if pending.scene.layers.iter().all(|layer| layer.ready) {
                        let pending = self.pending.take().expect("scene still pending");
                        self.finish(pending);
                    }
                }
                PlayerEvent::Error { message } => {
                    self.pending = None;
                    self.retry_at = Instant::now() + RETRY_INTERVAL;
                    self.notice(message, true);
                }
            }
        } else if self
            .scene
            .as_ref()
            .is_some_and(|scene| scene.generation == generation)
            && let PlayerEvent::Error { message } = event
        {
            self.scene = None;
            self.paused = false;
            self.retry_at = Instant::now() + RETRY_INTERVAL;
            self.notice(message, true);
        }
    }

    fn finish(&mut self, pending: Pending) {
        let targets = desktop::targets(&self.displays, &pending.active.settings.monitor);
        if !pending.scene.healthy() || !pending.scene.displays.iter().eq(targets) {
            self.notice(
                "Рабочий стол изменился во время установки. Попробуйте ещё раз.",
                true,
            );
            return;
        }
        if pending.save {
            let mut config = self.config.clone();
            config.draft = pending.active.settings.clone();
            config.active = Some(pending.active);
            if !self.save(config) {
                return;
            }
            self.paused = false;
        }
        pending.scene.play(!self.paused);
        self.scene = Some(pending.scene);
        self.render();
    }

    fn check(&mut self, target: &EventLoopWindowTarget<AppEvent>) {
        let displays = desktop::displays(target);
        let changed = displays != self.displays;
        self.displays = displays;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| Instant::now() >= pending.deadline)
        {
            self.pending = None;
            self.retry_at = Instant::now() + RETRY_INTERVAL;
            self.notice(
                "Обои не загрузились вовремя. Проверьте, что файл доступен.",
                true,
            );
        }
        if self.scene.as_ref().is_some_and(|scene| !scene.healthy()) {
            self.scene = None;
            self.paused = false;
            self.render();
        }
        if let Some(active) = self.config.active.clone()
            && self.pending.is_none()
            && !self.paused
            && (changed || Instant::now() >= self.retry_at)
        {
            let selected = desktop::targets(&self.displays, &active.settings.monitor);
            if selected.is_empty() {
                if self.scene.take().is_some() {
                    self.render();
                }
            } else if !self
                .scene
                .as_ref()
                .is_some_and(|scene| scene.displays.iter().eq(selected))
                && self.begin(active, false).is_ok()
            {
                self.render();
            }
        }
        if changed {
            self.render();
        }
    }
}
