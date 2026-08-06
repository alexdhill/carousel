mod agent;
mod app;
mod bundle;
mod commands;
mod config;
mod deck;
mod error;
mod export;
mod fonts;
mod html;
mod ipc;
mod present;
mod recents;

use app::ApplicationCore;
use bundle::{IoResponse, IoThread};
use deck::Deck;
use ipc::IpcMessage;
use ipc::bridge::WebviewSender;
use ipc::landing::{LandingData, LandingInbound, LandingRecent, LandingTemplate, ThumbData};
use ipc::present::PresentInbound;
use std::path::PathBuf;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopWindowTarget},
    window::{Fullscreen, Window, WindowBuilder},
};
use tracing::{error, info, warn};
use wry::{WebView, WebViewBuilder};

const HOST_HTML_TEMPLATE: &str = include_str!("../assets/host.html");
const HOST_CSS: &str = include_str!("../assets/host.css");
const HOST_JS: &str = include_str!("../assets/host.js");
const SNAP_JS: &str = include_str!("../assets/snap.js");
const CROP_JS: &str = include_str!("../assets/crop.js");
const STYLE_PROPS_JS: &str = include_str!("../assets/style_props.js");
const PRESET_CSS_JS: &str = include_str!("../assets/preset_css.js");
const PRESENT_HTML_TEMPLATE: &str = include_str!("../assets/present.html");
const PRESENT_CSS: &str = include_str!("../assets/present.css");
const PRESENT_JS: &str = include_str!("../assets/present.js");
const MORPH_JS: &str = include_str!("../assets/morph.js");
const LANDING_HTML_TEMPLATE: &str = include_str!("../assets/landing.html");
const LANDING_CSS: &str = include_str!("../assets/landing.css");
const LANDING_JS: &str = include_str!("../assets/landing.js");

#[derive(Debug)]
enum UserEvent {
    IpcReceived,
    FlushPatches,
    IoResponse,

    OpenPresentation,
    PresentIpcReceived,
    ClosePresentation,

    PdfRenderDone { ok: bool, dest: std::path::PathBuf },
    ChromiumProgress { received: u64, total: Option<u64> },
    ChromiumDone { ok: bool, message: String },

    LandingIpcReceived,
    LandingThumbReady,

    AgentEvent(crate::agent::AgentEvent),
}

enum ChromeJob {
    Render(app::PdfJob),
    Download,
}

fn write_pdf_atomic(dest: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp: std::path::PathBuf = dest.with_extension("pdf.partial");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dest)
}

fn spawn_chrome_worker(
    rx: std::sync::mpsc::Receiver<ChromeJob>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) {
    std::thread::spawn(move || {
        for job in rx {
            match job {
                ChromeJob::Render(j) => {
                    let ok: bool = match export::chromium::render_pdf(&j.chrome, &j.html, &j.raster)
                    {
                        Ok(bytes) => write_pdf_atomic(&j.dest, &bytes)
                            .map_err(|e| error!("pdf write failed: {}", e))
                            .is_ok(),
                        Err(e) => {
                            error!("pdf render failed: {}", e);
                            false
                        }
                    };
                    let _ = proxy.send_event(UserEvent::PdfRenderDone { ok, dest: j.dest });
                }
                ChromeJob::Download => {
                    let p = proxy.clone();
                    let progress = move |received: u64, total: Option<u64>| {
                        let _ = p.send_event(UserEvent::ChromiumProgress { received, total });
                    };
                    match export::chromium::download_chromium(&progress) {
                        Ok((path, revision)) => {
                            let mut cfg = config::load();
                            cfg.chrome_path = Some(path);
                            cfg.chromium_revision = Some(revision);
                            let _ = config::save(&cfg);
                            let _ = proxy.send_event(UserEvent::ChromiumDone {
                                ok: true,
                                message: String::new(),
                            });
                        }
                        Err(e) => {
                            let _ = proxy.send_event(UserEvent::ChromiumDone {
                                ok: false,
                                message: e.to_string(),
                            });
                        }
                    }
                }
            }
        }
    });
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter: EnvFilter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("carousel=info"));
    fmt().with_env_filter(filter).with_target(false).init();
}

#[cfg(target_os = "macos")]
fn ns_string(s: &str) -> *mut objc::runtime::Object {
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CString;
    let c: CString = CString::new(s).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

#[cfg(target_os = "macos")]
fn menu_item(title: &str, action: objc::runtime::Sel, key: &str) -> *mut objc::runtime::Object {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let item: *mut Object = msg_send![class!(NSMenuItem), alloc];
        msg_send![
            item,
            initWithTitle: ns_string(title)
            action: action
            keyEquivalent: ns_string(key)
        ]
    }
}

#[cfg(target_os = "macos")]
fn install_main_menu() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        assert!(
            !app.is_null(),
            "NSApplication sharedApplication returned nil"
        );
        let main_menu: *mut Object = msg_send![class!(NSMenu), new];
        assert!(!main_menu.is_null(), "NSMenu new returned nil");

        let app_item: *mut Object = msg_send![class!(NSMenuItem), new];
        let app_menu: *mut Object = msg_send![class!(NSMenu), new];

        let quit: *mut Object = menu_item("Quit", sel!(performClose:), "q");
        let _: () = msg_send![app_menu, addItem: quit];
        let _: () = msg_send![quit, release];
        let _: () = msg_send![app_item, setSubmenu: app_menu];
        let _: () = msg_send![app_menu, release];
        let _: () = msg_send![main_menu, addItem: app_item];
        let _: () = msg_send![app_item, release];

        let win_item: *mut Object = msg_send![class!(NSMenuItem), new];
        let win_menu_alloc: *mut Object = msg_send![class!(NSMenu), alloc];
        let win_menu: *mut Object = msg_send![win_menu_alloc, initWithTitle: ns_string("Window")];
        let minimize: *mut Object = menu_item("Minimize", sel!(performMiniaturize:), "m");
        let _: () = msg_send![win_menu, addItem: minimize];
        let _: () = msg_send![minimize, release];
        let close: *mut Object = menu_item("Close", sel!(performClose:), "w");
        let _: () = msg_send![win_menu, addItem: close];
        let _: () = msg_send![close, release];
        let _: () = msg_send![win_item, setSubmenu: win_menu];

        let _: () = msg_send![app, setWindowsMenu: win_menu];
        let _: () = msg_send![win_menu, release];
        let _: () = msg_send![main_menu, addItem: win_item];
        let _: () = msg_send![win_item, release];

        let _: () = msg_send![app, setMainMenu: main_menu];
        let _: () = msg_send![main_menu, release];
    }
}

fn assemble_host_html(
    template: &str,
    css: &str,
    js: &str,
    snap: &str,
    crop: &str,
    style_props: &str,
    preset_css: &str,
) -> String {
    assert!(
        template.contains("__HOST_CSS__"),
        "template missing CSS marker"
    );
    assert!(
        template.contains("__HOST_JS__"),
        "template missing JS marker"
    );
    assert!(
        template.contains("__SNAP_JS__"),
        "template missing snap JS marker"
    );
    assert!(
        template.contains("__CROP_JS__"),
        "template missing crop JS marker"
    );
    assert!(
        template.contains("__STYLE_PROPS_JS__"),
        "template missing style-props JS marker"
    );
    assert!(
        template.contains("__PRESET_CSS_JS__"),
        "template missing preset-css JS marker"
    );
    template
        .replace("__HOST_CSS__", css)
        .replace("__CROP_JS__", crop)
        .replace("__SNAP_JS__", snap)
        .replace("__STYLE_PROPS_JS__", style_props)
        .replace("__PRESET_CSS_JS__", preset_css)
        .replace("__HOST_JS__", js)
}

fn assemble_present_html(template: &str, css: &str, morph: &str, js: &str) -> String {
    assert!(
        template.contains("__PRESENT_CSS__"),
        "present template missing CSS marker"
    );
    assert!(
        template.contains("__MORPH_JS__"),
        "present template missing morph JS marker"
    );
    assert!(
        template.contains("__PRESENT_JS__"),
        "present template missing JS marker"
    );
    template
        .replace("__PRESENT_CSS__", css)
        .replace("__MORPH_JS__", morph)
        .replace("__PRESENT_JS__", js)
}

fn build_presentation(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    present_tx: std::sync::mpsc::Sender<PresentInbound>,
) -> Result<(Window, WebView), Box<dyn std::error::Error>> {
    let window = WindowBuilder::new()
        .with_title("carousel — presenting")
        .with_fullscreen(Some(Fullscreen::Borderless(None)))
        .build(target)?;
    let html: String =
        assemble_present_html(PRESENT_HTML_TEMPLATE, PRESENT_CSS, MORPH_JS, PRESENT_JS);
    assert!(!html.is_empty(), "assembled present html is empty");
    let webview = WebViewBuilder::new(&window)
        .with_html(html)
        .with_devtools(true)
        .with_ipc_handler(move |request: wry::http::Request<String>| {
            let body: &str = request.body();
            match serde_json::from_str::<PresentInbound>(body) {
                Ok(ctrl) => {
                    if present_tx.send(ctrl).is_err() {
                        error!("present ipc channel closed; dropping control");
                        return;
                    }
                    if proxy.send_event(UserEvent::PresentIpcReceived).is_err() {
                        error!("event loop proxy closed; cannot dispatch present control");
                    }
                }
                Err(e) => error!("present ipc parse error: {} body={}", e, body),
            }
        })
        .build()?;
    Ok((window, webview))
}

fn assemble_landing_html(template: &str, css: &str, js: &str) -> String {
    assert!(
        template.contains("__LANDING_CSS__"),
        "landing template missing CSS marker"
    );
    assert!(
        template.contains("__LANDING_JS__"),
        "landing template missing JS marker"
    );
    template
        .replace("__LANDING_CSS__", css)
        .replace("__LANDING_JS__", js)
}

fn build_landing(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    landing_tx: std::sync::mpsc::Sender<LandingInbound>,
) -> Result<(Window, WebView), Box<dyn std::error::Error>> {
    let window = WindowBuilder::new()
        .with_title("carousel")
        .with_inner_size(tao::dpi::LogicalSize::new(960.0, 640.0))
        .build(target)?;
    let html: String = assemble_landing_html(LANDING_HTML_TEMPLATE, LANDING_CSS, LANDING_JS);
    assert!(!html.is_empty(), "assembled landing html is empty");
    let webview = WebViewBuilder::new(&window)
        .with_html(html)
        .with_devtools(true)
        .with_ipc_handler(move |request: wry::http::Request<String>| {
            match serde_json::from_str::<LandingInbound>(request.body()) {
                Ok(inbound) => {
                    if landing_tx.send(inbound).is_err() {
                        error!("landing ipc channel closed; dropping control");
                        return;
                    }
                    if proxy.send_event(UserEvent::LandingIpcReceived).is_err() {
                        error!("event loop proxy closed; cannot dispatch landing control");
                    }
                }
                Err(e) => error!("landing ipc parse error: {} body={}", e, request.body()),
            }
        })
        .build()?;
    Ok((window, webview))
}

#[allow(clippy::too_many_arguments)]
fn build_editor(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    ipc_tx: std::sync::mpsc::Sender<IpcMessage>,
    deck: Deck,
    schedule_flush: Box<dyn Fn()>,
    io_thread: IoThread,
    request_present_open: Box<dyn Fn()>,
    request_present_close: Box<dyn Fn()>,
    dispatch_pdf_job: Box<dyn Fn(app::PdfJob)>,
    dispatch_chromium_download: Box<dyn Fn()>,
    agent_sink: std::sync::Arc<dyn Fn(crate::agent::AgentEvent) + Send + Sync>,
    focus_title: bool,
) -> Result<(Window, ApplicationCore), Box<dyn std::error::Error>> {
    let window = WindowBuilder::new()
        .with_title("carousel")
        .with_inner_size(tao::dpi::LogicalSize::new(1400.0, 900.0))
        .build(target)?;
    let html: String = assemble_host_html(
        HOST_HTML_TEMPLATE,
        HOST_CSS,
        HOST_JS,
        SNAP_JS,
        CROP_JS,
        STYLE_PROPS_JS,
        PRESET_CSS_JS,
    );
    assert!(!html.is_empty(), "assembled host html is empty");
    let webview = WebViewBuilder::new(&window)
        .with_html(html)
        .with_devtools(true)
        .with_ipc_handler(move |request: wry::http::Request<String>| {
            let body: &str = request.body();
            match serde_json::from_str::<IpcMessage>(body) {
                Ok(msg) => {
                    if ipc_tx.send(msg).is_err() {
                        error!("ipc channel closed; dropping message");
                        return;
                    }
                    if proxy.send_event(UserEvent::IpcReceived).is_err() {
                        error!("event loop proxy closed; cannot dispatch");
                    }
                }
                Err(e) => error!("ipc parse error: {} body={}", e, body),
            }
        })
        .build()?;
    let app = ApplicationCore::new_with_deck(
        deck,
        WebviewSender::new(webview),
        schedule_flush,
        io_thread,
        request_present_open,
        request_present_close,
        dispatch_pdf_job,
        dispatch_chromium_download,
        agent_sink,
        focus_title,
    );
    Ok((window, app))
}

fn send_landing(webview: &WebView, data: &LandingData) {
    let json: String = match serde_json::to_string(data) {
        Ok(j) => j,
        Err(e) => {
            error!("landing serialize failed: {}", e);
            return;
        }
    };
    let escaped: String = serde_json::to_string(&json).unwrap_or_else(|_| "\"\"".to_string());
    let script: String = format!("window.__landing.receive({});", escaped);
    if let Err(e) = webview.evaluate_script(&script) {
        error!("landing evaluate_script failed: {}", e);
    }
}

fn send_landing_thumb(webview: &WebView, path: &str, thumb: &ThumbData) {
    let payload: (&str, &ThumbData) = (path, thumb);
    let json: String = match serde_json::to_string(&payload) {
        Ok(j) => j,
        Err(e) => {
            error!("landing thumb serialize failed: {}", e);
            return;
        }
    };
    let escaped: String = serde_json::to_string(&json).unwrap_or_else(|_| "\"\"".to_string());
    let script: String = format!("window.__landing.thumb({});", escaped);
    if let Err(e) = webview.evaluate_script(&script) {
        error!("landing thumb evaluate_script failed: {}", e);
    }
}

fn spawn_landing_thumbs(
    paths: Vec<String>,
    sender: std::sync::mpsc::Sender<(String, ThumbData)>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) {
    assert!(paths.len() <= recents::CAP, "recents exceeded cap");
    let queue: std::sync::Arc<std::sync::Mutex<Vec<String>>> = {
        let mut remaining: Vec<String> = paths;
        remaining.reverse();
        std::sync::Arc::new(std::sync::Mutex::new(remaining))
    };
    let workers: usize = thumb_worker_count(queue.lock().map(|q| q.len()).unwrap_or(0));
    let mut w: usize = 0;
    while w < workers {
        w += 1;
        let queue = std::sync::Arc::clone(&queue);
        let sender = sender.clone();
        let proxy = proxy.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("carousel-thumbs-{}", w))
            .spawn(move || {
                let mut drawn: usize = 0;
                while drawn < recents::CAP {
                    drawn += 1;
                    let path: String = match queue.lock() {
                        Ok(mut q) => match q.pop() {
                            Some(p) => p,
                            None => return,
                        },
                        Err(_) => return,
                    };
                    let thumb = match html::thumbnail::build_thumb(std::path::Path::new(&path)) {
                        Some(t) => t,
                        None => continue,
                    };
                    if sender.send((path, thumb)).is_err() {
                        return;
                    }
                    if proxy.send_event(UserEvent::LandingThumbReady).is_err() {
                        return;
                    }
                }
            });
        if let Err(e) = spawned {
            error!("landing thumb thread spawn failed: {}", e);
        }
    }
}

// ponytail: capped at 4 because each worker holds a decoded full-size image
// (a 3840x2160 RGBA frame is ~33MB); raise it if peak memory stops mattering.
fn thumb_worker_count(jobs: usize) -> usize {
    const MAX_WORKERS: usize = 4;
    if jobs == 0 {
        return 0;
    }
    let cores: usize = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    cores.clamp(1, MAX_WORKERS).min(jobs)
}

fn landing_data() -> LandingData {
    let recents: Vec<LandingRecent> = recents::load_existing()
        .into_iter()
        .map(|r| LandingRecent {
            path: r.path,
            title: r.title,
            modified: r.modified,
            thumb: None,
        })
        .collect();
    let templates: Vec<LandingTemplate> = deck::templates::catalog()
        .into_iter()
        .map(|e| {
            let (background, foreground, accent) = deck::templates::theme_palette(&e.theme_id);
            LandingTemplate {
                theme_id: e.theme_id,
                theme_name: e.theme_name,
                layout_id: e.layout_id,
                layout_name: e.layout_name,
                background,
                foreground,
                accent,
            }
        })
        .collect();
    LandingData { recents, templates }
}

fn deck_for_open(inbound: &LandingInbound) -> Option<(Deck, Option<PathBuf>)> {
    use deck::templates::{light_theme, new_deck, new_deck_all_layouts, theme_by_id};
    match inbound {
        LandingInbound::OpenTemplate { theme_id, .. } => {
            Some((new_deck_all_layouts(theme_by_id(theme_id)), None))
        }
        LandingInbound::OpenRecent { path } => {
            Some((new_deck(light_theme(), "title"), Some(PathBuf::from(path))))
        }

        LandingInbound::OpenDefault => {
            let path: PathBuf = rfd::FileDialog::new()
                .add_filter("Slide Deck", &["deck", "slidedeck"])
                .pick_file()?;
            Some((new_deck(light_theme(), "title"), Some(path)))
        }
        _ => None,
    }
}

fn initial_open_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::args_os().nth(1)?);
    if path.is_file() { Some(path) } else { None }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    info!("starting carousel");

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let proxy_for_app = proxy.clone();
    let (ipc_tx, ipc_rx) = std::sync::mpsc::channel::<IpcMessage>();

    let (landing_tx, landing_rx) = std::sync::mpsc::channel::<LandingInbound>();
    let (thumb_tx, thumb_rx) = std::sync::mpsc::channel::<(String, ThumbData)>();

    let landing_tx_startup = landing_tx.clone();
    let landing_tx_open = landing_tx.clone();
    let (landing_win, landing_wv) = build_landing(&event_loop, proxy.clone(), landing_tx)?;
    let mut landing_window: Option<Window> = Some(landing_win);
    let mut landing_webview: Option<WebView> = Some(landing_wv);

    if let Some(path) = initial_open_path() {
        let _ = landing_tx_startup.send(LandingInbound::OpenRecent {
            path: path.to_string_lossy().into_owned(),
        });
        let _ = proxy.send_event(UserEvent::LandingIpcReceived);
    }

    let schedule_flush: Box<dyn Fn()> = {
        let p = proxy_for_app.clone();
        Box::new(move || {
            if p.send_event(UserEvent::FlushPatches).is_err() {
                error!("could not schedule FlushPatches; proxy closed");
            }
        })
    };

    let (present_tx, present_rx) = std::sync::mpsc::channel::<PresentInbound>();
    let request_present_open: Box<dyn Fn()> = {
        let p = proxy_for_app.clone();
        Box::new(move || {
            if p.send_event(UserEvent::OpenPresentation).is_err() {
                error!("could not schedule OpenPresentation; proxy closed");
            }
        })
    };
    let request_present_close: Box<dyn Fn()> = {
        let p = proxy_for_app.clone();
        Box::new(move || {
            if p.send_event(UserEvent::ClosePresentation).is_err() {
                error!("could not schedule ClosePresentation; proxy closed");
            }
        })
    };

    let (chrome_tx, chrome_rx) = std::sync::mpsc::channel::<ChromeJob>();
    spawn_chrome_worker(chrome_rx, proxy_for_app.clone());
    let dispatch_pdf_job: Box<dyn Fn(app::PdfJob)> = {
        let tx = chrome_tx.clone();
        Box::new(move |job| {
            if tx.send(ChromeJob::Render(job)).is_err() {
                error!("chrome worker gone; cannot render pdf");
            }
        })
    };
    let dispatch_chromium_download: Box<dyn Fn()> = {
        let tx = chrome_tx.clone();
        Box::new(move || {
            if tx.send(ChromeJob::Download).is_err() {
                error!("chrome worker gone; cannot download chromium");
            }
        })
    };

    let (io_tx, io_rx) = std::sync::mpsc::channel::<IoResponse>();
    let io_wake: Box<dyn Fn() + Send + 'static> = {
        let p = proxy_for_app.clone();
        Box::new(move || {
            if p.send_event(UserEvent::IoResponse).is_err() {
                error!("could not schedule IoResponse; proxy closed");
            }
        })
    };
    let io_thread: IoThread = IoThread::spawn(io_tx, io_wake)?;

    let agent_sink: std::sync::Arc<dyn Fn(crate::agent::AgentEvent) + Send + Sync> = {
        let p = proxy_for_app.clone();
        std::sync::Arc::new(move |ev| {
            let _ = p.send_event(UserEvent::AgentEvent(ev));
        })
    };

    let mut schedule_flush_opt: Option<Box<dyn Fn()>> = Some(schedule_flush);
    let mut io_thread_opt: Option<IoThread> = Some(io_thread);
    let mut request_present_open_opt: Option<Box<dyn Fn()>> = Some(request_present_open);
    let mut request_present_close_opt: Option<Box<dyn Fn()>> = Some(request_present_close);
    let mut dispatch_pdf_job_opt: Option<Box<dyn Fn(app::PdfJob)>> = Some(dispatch_pdf_job);
    let mut dispatch_chromium_download_opt: Option<Box<dyn Fn()>> =
        Some(dispatch_chromium_download);
    let mut agent_sink_opt: Option<std::sync::Arc<dyn Fn(crate::agent::AgentEvent) + Send + Sync>> =
        Some(agent_sink);
    let mut app: Option<ApplicationCore> = None;
    let mut editor_window: Option<Window> = None;

    #[cfg(target_os = "macos")]
    install_main_menu();

    let mut present_window: Option<Window> = None;
    let proxy_present = proxy_for_app.clone();

    info!("event loop running");
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::IpcReceived) => {
                while let Ok(msg) = ipc_rx.try_recv() {
                    if let Some(app) = app.as_mut()
                        && let Err(e) = app.handle_ipc(msg)
                    {
                        error!("handle_ipc failed: {}", e);
                    }
                }
                if app.as_mut().is_some_and(|a| a.take_quit_requested()) {
                    info!("quit confirmed; exiting");
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::FlushPatches) => {
                if let Some(app) = app.as_mut()
                    && let Err(e) = app.flush_patches()
                {
                    error!("flush_patches failed: {}", e);
                }
            }
            Event::UserEvent(UserEvent::IoResponse) => {
                while let Ok(resp) = io_rx.try_recv() {
                    if let Some(app) = app.as_mut()
                        && let Err(e) = app.handle_io_response(resp)
                    {
                        error!("handle_io_response failed: {}", e);
                    }
                }
                if app.as_mut().is_some_and(|a| a.take_quit_requested()) {
                    info!("save-and-exit committed; exiting");
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::OpenPresentation) => {
                if let Some(app) = app.as_mut() {
                    if present_window.is_some() {
                        warn!("OpenPresentation ignored; already presenting");
                    } else {
                        match build_presentation(target, proxy_present.clone(), present_tx.clone())
                        {
                            Ok((win, wv)) => {
                                app.begin_presentation(WebviewSender::new(wv));
                                present_window = Some(win);
                            }
                            Err(e) => error!("failed to build presentation window: {}", e),
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::PresentIpcReceived) => {
                while let Ok(ctrl) = present_rx.try_recv() {
                    if let Some(app) = app.as_mut()
                        && let Err(e) = app.handle_present_control(ctrl)
                    {
                        error!("handle_present_control failed: {}", e);
                    }
                }
            }
            Event::UserEvent(UserEvent::ClosePresentation) => {

                if let Some(app) = app.as_mut() {
                    app.end_presentation();
                }
                present_window = None;
            }
            Event::UserEvent(UserEvent::PdfRenderDone { ok, dest }) => {
                if let Some(app) = app.as_mut() {
                    app.notify_pdf_export(&dest, ok);
                }
            }
            Event::UserEvent(UserEvent::ChromiumProgress { received, total }) => {
                if let Some(app) = app.as_ref() {
                    app.send_chromium_progress(received, total);
                }
            }
            Event::UserEvent(UserEvent::ChromiumDone { ok, message }) => {
                if let Some(app) = app.as_mut() {
                    app.send_chromium_done(ok, message);
                    if ok {

                        app.on_chromium_ready();
                    }
                }
            }
            Event::UserEvent(UserEvent::LandingThumbReady) => {
                while let Ok((path, thumb)) = thumb_rx.try_recv() {
                    if let Some(wv) = landing_webview.as_ref() {
                        send_landing_thumb(wv, &path, &thumb);
                    }
                }
            }
            Event::UserEvent(UserEvent::LandingIpcReceived) => {
                while let Ok(inbound) = landing_rx.try_recv() {
                    match inbound {
                        LandingInbound::Ready => {
                            if let Some(wv) = landing_webview.as_ref() {
                                let data: LandingData = landing_data();
                                let paths: Vec<String> =
                                    data.recents.iter().map(|r| r.path.clone()).collect();
                                send_landing(wv, &data);
                                spawn_landing_thumbs(paths, thumb_tx.clone(), proxy.clone());
                            }
                        }
                        LandingInbound::Cancel => {
                            if app.is_none() {
                                info!("landing: cancelled; exiting");
                                *control_flow = ControlFlow::Exit;
                            }
                        }
                        LandingInbound::ForgetRecent { path } => {
                            info!("landing: forgetting recent");
                            recents::forget(&path);
                        }

                        open => {

                            let chosen = if app.is_some() {
                                warn!("landing open ignored; editor already open");
                                None
                            } else {
                                deck_for_open(&open)
                            };

                            if let Some((deck, load)) = chosen {

                                let focus_title: bool = load.is_none();
                                if let (
                                    Some(sf),
                                    Some(io),
                                    Some(rpo),
                                    Some(rpc),
                                    Some(dpj),
                                    Some(dcd),
                                    Some(as_),
                                ) = (
                                    schedule_flush_opt.take(),
                                    io_thread_opt.take(),
                                    request_present_open_opt.take(),
                                    request_present_close_opt.take(),
                                    dispatch_pdf_job_opt.take(),
                                    dispatch_chromium_download_opt.take(),
                                    agent_sink_opt.take(),
                                ) {
                                    match build_editor(
                                        target,
                                        proxy.clone(),
                                        ipc_tx.clone(),
                                        deck,
                                        sf,
                                        io,
                                        rpo,
                                        rpc,
                                        dpj,
                                        dcd,
                                        as_,
                                        focus_title,
                                    ) {
                                        Ok((win, mut a)) => {
                                            if let Some(path) = load {
                                                a.load_path(path);
                                            }
                                            app = Some(a);
                                            editor_window = Some(win);
                                            landing_window = None;
                                            landing_webview = None;
                                        }
                                        Err(e) => error!("failed to build editor: {}", e),
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::AgentEvent(ev)) => {
                if let Some(app) = app.as_mut()
                    && let Err(e) = app.handle_agent_event(ev)
                {
                    error!("agent event failed: {}", e);
                }
            }

            Event::Opened { urls } => {
                for u in &urls {
                    if let Ok(path) = u.to_file_path() {
                        let _ = landing_tx_open.send(LandingInbound::OpenRecent {
                            path: path.to_string_lossy().into_owned(),
                        });
                        let _ = proxy.send_event(UserEvent::LandingIpcReceived);
                    }
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } => {
                if Some(window_id) == present_window.as_ref().map(|w| w.id()) {
                    info!("presentation window closed; ending presentation");
                    if let Some(app) = app.as_mut() {
                        app.end_presentation();
                    }
                    present_window = None;
                } else if Some(window_id) == editor_window.as_ref().map(|w| w.id()) {
                    match app.as_ref() {
                        Some(a) if a.wants_quit_confirmation() => {
                            info!("editor close requested with unsaved changes; confirming");
                            if let Err(e) = a.show_quit_dialog() {
                                error!("show_quit_dialog failed: {}", e);
                            }
                        }
                        _ => {
                            info!("editor closed; exiting");
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                } else if Some(window_id) == landing_window.as_ref().map(|w| w.id()) {
                    info!("landing closed; exiting");
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_workers_stay_within_jobs_and_cap() {
        assert_eq!(thumb_worker_count(0), 0, "no jobs means no threads");
        assert_eq!(thumb_worker_count(1), 1, "one job never oversubscribes");
        let many: usize = thumb_worker_count(recents::CAP);
        assert!((1..=4).contains(&many), "worker count out of range: {}", many);
    }

    #[test]
    fn landing_data_defers_every_thumbnail() {
        let data: LandingData = landing_data();
        assert!(
            data.recents.len() <= recents::CAP,
            "recents exceeded cap: {}",
            data.recents.len()
        );
        for r in &data.recents {
            assert!(r.thumb.is_none(), "landing_data must not build thumbs inline");
        }
    }
}
