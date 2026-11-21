use crate::{
    library::{self, Library},
    media::{self, Recorder, Settings},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::{EventLoopBuilderExtWindows, WindowExtWindows},
    window::{Window, WindowBuilder},
};
use tray_icon::{
    MouseButton, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};
use windows::{
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND},
        System::Threading::CreateMutexW,
        UI::{
            Input::KeyboardAndMouse::{
                MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
            },
            WindowsAndMessaging::{MSG, WM_HOTKEY},
        },
    },
    core::PCWSTR,
};
use wry::{WebContext, WebView, WebViewBuilder};
const SHOW_STUDIO: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 0x41;

type Done = std::result::Result<(PathBuf, String, bool), String>;
#[derive(Debug)]
enum JobKind {
    Replay,
    Edit,
}
#[derive(Debug)]
enum Action {
    Cmd(Value),
    Open,
    Exit,
    Tick,
    Done(JobKind, Done),
    Art(String, String),
}
fn post(proxy: &EventLoopProxy<Action>, a: Action) {
    let _ = proxy.send_event(a);
}
fn bind(window: &Window, key: u32) -> bool {
    unsafe {
        RegisterHotKey(
            Some(HWND(window.hwnd() as _)),
            1,
            MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
            0x70 + key - 1,
        )
        .is_ok()
    }
}
fn unbind(window: &Window) {
    unsafe {
        let _ = UnregisterHotKey(Some(HWND(window.hwnd() as _)), 1);
    }
}
fn show(window: &Window) {
    window.set_visible(true);
    window.set_minimized(false);
    window.set_focus();
}
fn js(web: &WebView, func: &str, data: Value) {
    let _ = web.evaluate_script(&format!("window.{func}?.({data})"));
}
struct Studio {
    lib: Library,
    s: Settings,
    rec: Option<Recorder>,
    wants: bool,
    ready: usize,
    busy: bool,
    saving: bool,
    bound: bool,
    message: String,
    retry: Instant,
    proxy: EventLoopProxy<Action>,
    art: mpsc::Sender<(PathBuf, String)>,
}
impl Studio {
    fn start(&mut self, w: &Window) -> Result<()> {
        let monitors = w.available_monitors().collect::<Vec<_>>();
        let m = monitors
            .get(self.s.monitor)
            .or_else(|| monitors.first())
            .context("No display is available")?;
        let p = m.position();
        let d = m.size();
        self.rec = Some(Recorder::start(
            &self.lib.root,
            &self.s,
            (p.x, p.y, d.width, d.height),
        )?);
        Ok(())
    }
    fn tick(&mut self, w: &Window) {
        if let Some(r) = &mut self.rec {
            match r.tick() {
                Ok(n) => self.ready = n,
                Err(e) => {
                    self.rec = None;
                    self.ready = 0;
                    self.message = e.to_string();
                    self.retry = Instant::now() + Duration::from_secs(8)
                }
            }
        } else if self.wants && Instant::now() >= self.retry {
            self.retry = Instant::now() + Duration::from_secs(8);
            if let Err(e) = self.start(w) {
                self.message = e.to_string()
            }
        }
    }
    fn state(&self, w: &Window) -> Value {
        let displays = w
            .available_monitors()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "Display {} · {} × {}",
                    i + 1,
                    m.size().width,
                    m.size().height
                )
            })
            .collect::<Vec<_>>();
        json!({"maximized":w.is_maximized(),"settings":self.s,"running":self.rec.is_some(),"wants":self.wants,"ready":self.ready,"busy":self.busy,"saving":self.saving,"bound":self.bound,"message":self.message,"displays":displays,"audioError":self.rec.as_ref().map(Recorder::audio_error).unwrap_or_default()})
    }
    fn work(
        &mut self,
        kind: JobKind,
        f: impl FnOnce() -> Result<(PathBuf, String, bool)> + Send + 'static,
    ) {
        match kind {
            JobKind::Replay => self.saving = true,
            JobKind::Edit => self.busy = true,
        }
        let proxy = self.proxy.clone();
        thread::spawn(move || post(&proxy, Action::Done(kind, f().map_err(|e| e.to_string()))));
    }
    fn command(&mut self, j: Value, w: &Window, web: &WebView) -> Result<()> {
        let cmd = j["cmd"].as_str().unwrap_or("");
        let key = j["id"].as_str().unwrap_or("");
        let clip = self
            .lib
            .clips
            .iter()
            .find(|c| library::id(&c.file) == key)
            .cloned();
        match cmd {
            "window-drag" => {
                w.drag_window()?;
            }
            "window-minimize" => w.set_minimized(true),
            "window-maximize" => w.set_maximized(!w.is_maximized()),
            "window-hide" => {
                let _ = web.evaluate_script("document.querySelector('video')?.pause()");
                w.set_visible(false);
            }
            "window-resize" => {
                use tao::window::ResizeDirection::*;
                let edge = match j["edge"].as_str() {
                    Some("n") => North,
                    Some("s") => South,
                    Some("e") => East,
                    Some("w") => West,
                    Some("nw") => NorthWest,
                    Some("ne") => NorthEast,
                    Some("sw") => SouthWest,
                    Some("se") => SouthEast,
                    _ => bail!("Unknown window edge"),
                };
                if !w.is_maximized() {
                    w.drag_resize_window(edge)?;
                }
            }
            "init" => js(web, "library", json!(self.lib.rows())),
            "toggle" => {
                self.wants = !self.wants;
                self.rec = None;
                self.ready = 0;
                self.retry = Instant::now();
                self.message.clear();
                self.tick(w);
            }
            "save" => {
                ensure!(!self.saving, "Your previous replay is still saving.");
                let snap = self
                    .rec
                    .as_ref()
                    .context("Recording is paused")?
                    .snapshot(self.s.seconds as usize, &self.lib.root)?;
                let target = self.lib.target("Replay");
                self.message = "Saving replay…".into();
                self.work(JobKind::Replay, move || {
                    snap.finish(&target)?;
                    Ok((
                        target,
                        chrono::Local::now()
                            .format("Replay · %b %d, %H:%M:%S")
                            .to_string(),
                        true,
                    ))
                });
            }
            "settings" => {
                let s: Settings = serde_json::from_value(j["settings"].clone())?;
                ensure!(
                    s.valid() && s.monitor < w.available_monitors().count(),
                    "Choose valid capture settings."
                );
                if s.key != self.s.key || !self.bound {
                    unbind(w);
                    if !bind(w, s.key) {
                        self.bound = bind(w, self.s.key);
                        bail!("That shortcut is in use. Choose another function key.")
                    }
                    self.bound = true;
                }
                library::write(&self.lib.root.join("settings.json"), &s)?;
                let capture_changed = (self.s.audio, self.s.fps, self.s.height, self.s.monitor)
                    != (s.audio, s.fps, s.height, s.monitor);
                self.s = s;
                if capture_changed {
                    self.rec = None;
                    self.ready = 0;
                    self.retry = Instant::now();
                }
                self.message = "Settings saved.".into();
                self.tick(w);
            }
            "folder" => {
                std::process::Command::new("explorer.exe")
                    .arg(&self.lib.videos)
                    .spawn()?;
            }
            "rename" | "star" => {
                let c = self
                    .lib
                    .clips
                    .iter_mut()
                    .find(|c| library::id(&c.file) == key)
                    .context("Clip missing")?;
                if cmd == "rename" {
                    let t = j["title"].as_str().unwrap_or("").trim();
                    ensure!(
                        !t.is_empty() && t.chars().count() <= 120,
                        "Enter a title up to 120 characters."
                    );
                    if let Some(value) = j.get("tags") {
                        let values = value.as_array().context("Invalid tags")?;
                        ensure!(values.len() <= 12, "Use up to 12 tags per clip.");
                        let mut tags: Vec<String> = Vec::new();
                        for value in values {
                            let tag = value.as_str().context("Invalid tag")?.trim();
                            ensure!(
                                tag.chars().count() <= 32,
                                "Use up to 32 characters per tag."
                            );
                            if !tag.is_empty()
                                && !tags
                                    .iter()
                                    .any(|existing| existing.eq_ignore_ascii_case(tag))
                            {
                                tags.push(tag.into());
                            }
                        }
                        c.tags = tags;
                    }
                    c.title = t.into()
                } else {
                    c.star = !c.star
                }
                self.lib.persist()?;
                js(web, "library", json!(self.lib.rows()));
            }
            "draft" => {
                let c = clip.context("Clip missing")?;
                self.lib.cuts.insert(c.file, j["draft"].clone());
                library::write(&self.lib.root.join("cuts.json"), &self.lib.cuts)?;
            }
            "strip" => {
                if let Some(c) = clip {
                    let _ = self.art.send((c.file, "strip".into()));
                }
            }
            "import" => {
                ensure!(!self.busy, "Wait for the current media operation.");
                if let Some(input) = rfd::FileDialog::new()
                    .add_filter("Video", &["mp4", "mov", "mkv", "webm", "avi", "m4v"])
                    .pick_file()
                {
                    let target = self.lib.target("Import");
                    self.message = "Importing video…".into();
                    self.work(JobKind::Edit, move || {
                        media::export(
                            &input,
                            &target,
                            0.0,
                            media::duration(&input)?,
                            0,
                            false,
                            media::Quality::Balanced,
                        )?;
                        let title = input
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        Ok((target, title, true))
                    })
                }
            }
            "export" => {
                ensure!(!self.busy, "Wait for the current media operation.");
                let c = clip.context("Clip missing")?;
                let start = j["start"].as_f64().context("Choose a start time")?;
                let end = j["end"].as_f64().context("Choose an end time")?;
                let shape = j["shape"].as_u64().unwrap_or(0) as u32;
                let mute = j["mute"] == true;
                let quality = serde_json::from_value(j["quality"].clone()).unwrap_or_default();
                let name = c
                    .title
                    .chars()
                    .map(|x| if "<>:\"/\\|?*".contains(x) { '_' } else { x })
                    .collect::<String>();
                if let Some(target) = rfd::FileDialog::new()
                    .set_directory(&self.lib.videos)
                    .set_file_name(format!("{name} - edit.mp4"))
                    .add_filter("MP4", &["mp4"])
                    .save_file()
                {
                    let target = target.with_extension("mp4");
                    ensure!(
                        !self.lib.clips.iter().any(|x| x
                            .file
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&target.to_string_lossy())),
                        "Choose a new filename to preserve the original clip."
                    );
                    let add = target.parent() == Some(&self.lib.videos);
                    self.message = "Exporting selection…".into();
                    self.work(JobKind::Edit, move || {
                        media::export(&c.file, &target, start, end, shape, mute, quality)?;
                        let title = target
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        Ok((target, title, add))
                    })
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn art_worker(dir: PathBuf, proxy: EventLoopProxy<Action>) -> mpsc::Sender<(PathBuf, String)> {
    let (tx, rx) = mpsc::channel::<(PathBuf, String)>();
    thread::spawn(move || {
        for (path, kind) in rx {
            let id = library::id(&path);
            let out = dir.join(format!("{id}-{kind}.jpg"));
            if !out.exists() {
                let mut cmd = media::command("ffmpeg");
                cmd.args(["-v", "error", "-y"]);
                if kind == "thumb" {
                    cmd.args(["-i"]).arg(&path).args(["-vf", "scale=360:-2"]);
                } else {
                    let Ok(d) = media::duration(&path) else {
                        continue;
                    };
                    cmd.arg("-i").arg(&path).args(["-vf",&format!("fps=10/{:.3}:start_time=0:round=up,scale=128:72:force_original_aspect_ratio=increase,crop=128:72,tile=10x1",d.max(0.01))]);
                }
                let temp = out.with_extension("tmp.jpg");
                if media::run(cmd.args(["-an", "-frames:v", "1"]).arg(&temp)).is_err()
                    || fs::rename(&temp, &out).is_err()
                {
                    let _ = fs::remove_file(temp);
                    continue;
                }
            }
            post(&proxy, Action::Art(id, kind));
        }
    });
    tx
}
fn icon(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let i = image::load_from_memory_with_format(bytes, image::ImageFormat::Ico)?.to_rgba8();
    let (w, h) = i.dimensions();
    Ok((i.into_raw(), w, h))
}
pub fn run() -> Result<()> {
    let root = std::env::var_os("AFTERIMAGE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or(
            dirs::data_local_dir()
                .context("Missing Windows application data directory")?
                .join("Afterimage"),
        );
    let videos = std::env::var_os("AFTERIMAGE_CLIPS_DIR")
        .map(PathBuf::from)
        .unwrap_or(
            dirs::video_dir()
                .context("Missing Windows Videos folder")?
                .join("Afterimage"),
        );
    let name = if std::env::var_os("AFTERIMAGE_DATA_DIR").is_some() {
        format!("Local\\Afterimage.Replay.{}", library::id(&root))
    } else {
        "Local\\Afterimage.Replay".into()
    };
    let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let _instance = unsafe {
        let h = CreateMutexW(None, false, PCWSTR(wide.as_ptr()))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if std::env::args().any(|a| a == "--show") {
                use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
                if let Ok(existing) =
                    FindWindowW(None, windows::core::w!("Afterimage • Replay studio"))
                {
                    PostMessageW(
                        Some(existing),
                        SHOW_STUDIO,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    )?;
                }
            }
            return Ok(());
        }
        h
    };
    let lib = Library::load(root.clone(), videos)?;
    let mut settings: Settings = library::read(&root.join("settings.json"))?;
    if !settings.valid() {
        settings = Settings::default();
    }
    // Only folders created by Rust capture jobs live below these two directories.
    for parent in ["rust-buffer", "rust-jobs"] {
        if let Ok(d) = fs::read_dir(root.join(parent)) {
            for e in d.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir())
                    && e.file_name()
                        .to_string_lossy()
                        .split_once('-')
                        .is_some_and(|(a, b)| a.parse::<u128>().is_ok() && b.parse::<u64>().is_ok())
                {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
    }
    let mut builder = EventLoopBuilder::<Action>::with_user_event();
    let hook: Arc<Mutex<Option<EventLoopProxy<Action>>>> = Arc::default();
    let h = hook.clone();
    builder.with_msg_hook(move |p| {
        let m = unsafe { &*(p as *const MSG) };
        if m.message == SHOW_STUDIO {
            if let Some(proxy) = h.lock().unwrap().as_ref() {
                post(proxy, Action::Open);
            }
            true
        } else if m.message == WM_HOTKEY && m.wParam.0 == 1 {
            if let Some(proxy) = h.lock().unwrap().as_ref() {
                post(proxy, Action::Cmd(json!({"cmd":"save"})));
            }
            true
        } else {
            false
        }
    });
    let ev = builder.build();
    let proxy = ev.create_proxy();
    *hook.lock().unwrap() = Some(proxy.clone());
    let (i, w, h) = icon(include_bytes!("../assets/Afterimage.ico"))?;
    let window = WindowBuilder::new()
        .with_title("Afterimage • Replay studio")
        .with_decorations(false)
        .with_window_icon(Some(tao::window::Icon::from_rgba(i, w, h)?))
        .with_visible(false)
        .with_inner_size(LogicalSize::new(1140, 780))
        .with_min_inner_size(LogicalSize::new(800, 600))
        .build(&ev)?;
    window.set_undecorated_shadow(true);
    let files = lib.files.clone();
    let base = root.clone();
    let ipc = proxy.clone();
    let mut ctx = WebContext::new(Some(root.join("webview")));
    let web = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_custom_protocol("studio".into(), move |_, req| {
            library::serve(req, &files, &base).map(Into::into)
        })
        .with_navigation_handler(|u| {
            u == "http://studio.localhost/"
                || u == "studio://localhost/"
                || u == "studio://localhost"
                || u == "about:blank"
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_ipc_handler(move |r| {
            if let Ok(j) = serde_json::from_str(r.body()) {
                post(&ipc, Action::Cmd(j));
            }
        })
        .with_url("studio://localhost/")
        .build(&window)?;
    let menu = Menu::new();
    let open = MenuItem::with_id("open", "Open Afterimage", true, None);
    let save = MenuItem::with_id("save", "Save replay", true, None);
    let toggle = MenuItem::with_id("toggle", "Pause recording", true, None);
    let quit = MenuItem::with_id("exit", "Exit", true, None);
    menu.append_items(&[&open, &save, &toggle, &quit])?;
    let (i, w, h) = icon(include_bytes!("../assets/Tray.ico"))?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Afterimage")
        .with_icon(tray_icon::Icon::from_rgba(i, w, h)?)
        .build()?;
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let id = e.id.0.as_str();
        post(
            &p,
            match id {
                "open" => Action::Open,
                "exit" => Action::Exit,
                _ => Action::Cmd(json!({"cmd":id})),
            },
        )
    }));
    let p = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e| {
        if matches!(
            e,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        ) {
            post(&p, Action::Open)
        }
    }));
    let bound = bind(&window, settings.key);
    let art = art_worker(root.join("thumbs"), proxy.clone());
    for c in &lib.clips {
        let _ = art.send((c.file.clone(), "thumb".into()));
    }
    let mut app = Studio {
        lib,
        s: settings,
        rec: None,
        wants: true,
        ready: 0,
        busy: false,
        saving: false,
        bound,
        message: if bound {
            String::new()
        } else {
            "Save shortcut is in use. Choose another in Settings.".into()
        },
        retry: Instant::now(),
        proxy: proxy.clone(),
        art,
    };
    app.tick(&window);
    if std::env::args().any(|a| a == "--show") {
        show(&window);
    }
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(1));
            if proxy.send_event(Action::Tick).is_err() {
                break;
            }
        }
    });
    ev.run(move |e, _, cf| {
        *cf = ControlFlow::Wait;
        match e {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let _ = web.evaluate_script("document.querySelector('video')?.pause()");
                window.set_visible(false);
            }
            Event::UserEvent(event) => {
                match event {
                    Action::Open => show(&window),
                    Action::Tick => app.tick(&window),
                    Action::Exit => {
                        if app.busy || app.saving {
                            app.message = "Finish the current save or export before closing.".into()
                        } else {
                            app.wants = false;
                            app.rec = None;
                            unbind(&window);
                            *cf = ControlFlow::Exit;
                        }
                    }
                    Action::Cmd(j) => {
                        if let Err(e) = app.command(j, &window, &web) {
                            app.message = e.to_string()
                        }
                    }
                    Action::Art(id, kind) => {
                        let _ = web.evaluate_script(&format!(
                            "window.art?.({}, {})",
                            json!(id),
                            json!(kind)
                        ));
                    }
                    Action::Done(kind, r) => {
                        match kind {
                            JobKind::Replay => app.saving = false,
                            JobKind::Edit => app.busy = false,
                        }
                        match r {
                            Ok((path, title, add)) => {
                                app.message = format!("Saved {}", path.display());
                                if add {
                                    if let Err(e) = app.lib.add(path.clone(), title) {
                                        app.message = e.to_string();
                                    }
                                    let _ = app.art.send((path, "thumb".into()));
                                    js(&web, "library", json!(app.lib.rows()));
                                }
                            }
                            Err(e) => app.message = e,
                        }
                    }
                }
                toggle.set_text(if app.wants {
                    "Pause recording"
                } else {
                    "Start recording"
                });
                save.set_enabled(app.ready > 0 && !app.saving);
                let _ = tray.set_tooltip(Some(if app.rec.is_some() {
                    "Afterimage · Recording"
                } else {
                    "Afterimage · Recording off"
                }));
                js(&web, "update", app.state(&window));
            }
            Event::LoopDestroyed => {
                app.rec = None;
            }
            _ => {}
        }
    });
}
