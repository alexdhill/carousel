mod app;
mod bundle;
mod commands;
mod deck;
mod error;
mod html;
mod ipc;
mod present;

use app::ApplicationCore;
use bundle::{IoResponse, IoThread};
use ipc::IpcMessage;
use ipc::bridge::WebviewSender;
use ipc::present::PresentInbound;
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
const PRESENT_HTML_TEMPLATE: &str = include_str!("../assets/present.html");
const PRESENT_CSS: &str = include_str!("../assets/present.css");
const PRESENT_JS: &str = include_str!("../assets/present.js");

// UserEvent
// Custom events injected into the Tao event loop.
// - IpcReceived: the off-thread IPC handler pushed a message onto the
//   channel; main thread should drain.
// - FlushPatches: a command dispatch left the patch buffer non-empty;
//   main thread should coalesce + send on the next iteration.
// - IoResponse:  the bundle I/O worker thread posted an IoResponse onto
//   its channel; main thread should drain and hand each to the app.
#[derive(Debug, Clone, Copy)]
enum UserEvent {
    IpcReceived,
    FlushPatches,
    IoResponse,
    // Presentation mode. The app asked the event loop to build the fullscreen
    // presentation window (OpenPresentation), the presentation webview pushed a
    // control onto its channel (PresentIpcReceived), or the app asked to tear
    // the window down (ClosePresentation). Window creation/teardown lives in the
    // run closure because it needs the EventLoopWindowTarget.
    OpenPresentation,
    PresentIpcReceived,
    ClosePresentation,
}

// init_tracing
// Inputs: none. Reads RUST_LOG from the environment.
// Output: side-effect; installs the global tracing subscriber.
// Errors: silently falls back to a sensible default if RUST_LOG is absent
// or unparseable.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter: EnvFilter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("carousel=info"));
    fmt().with_env_filter(filter).with_target(false).init();
}

// ns_string (macOS)
// Inputs: a Rust &str. Output: an autoreleased NSString carrying the same
// text. Used for menu titles and key equivalents.
#[cfg(target_os = "macos")]
fn ns_string(s: &str) -> *mut objc::runtime::Object {
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CString;
    let c: CString = CString::new(s).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

// menu_item (macOS)
// Inputs: a title, a standard AppKit action selector, and a single-
// character key equivalent (Command is the implicit modifier for menu
// items). Output: a retained (+1) NSMenuItem; the caller adds it to a menu
// (which retains it) and then releases this reference.
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

// install_main_menu (macOS)
// Inputs: none. Output: side-effect; installs a minimal application main
// menu with the standard macOS accelerators:
//   • Quit     — Cmd+Q  (terminate:)
//   • Minimize — Cmd+M  (performMiniaturize:)
//   • Close    — Cmd+W  (performClose:)
// Why: a main menu is also required to avoid wry 0.45's keyDown forwarding
// dereferencing a null `[NSApp mainMenu]`, so this real menu both restores
// the expected Mac shortcuts AND is the valid performKeyEquivalent: target
// for every other (unmapped) key. Standard AppKit selectors mean AppKit
// does the work — terminate: quits, performClose:/performMiniaturize: act
// on the key window.
// Control flow: build the app + window submenus, attach their items, hang
// both off the main menu, install it. Each object is released once after
// its parent has retained it, so the app holds the only references.
// Must run after the NSApplication exists (i.e. after the window/event
// loop is built).
#[cfg(target_os = "macos")]
fn install_main_menu() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        assert!(!app.is_null(), "NSApplication sharedApplication returned nil");
        let main_menu: *mut Object = msg_send![class!(NSMenu), new];
        assert!(!main_menu.is_null(), "NSMenu new returned nil");

        // Application menu — Quit.
        let app_item: *mut Object = msg_send![class!(NSMenuItem), new];
        let app_menu: *mut Object = msg_send![class!(NSMenu), new];
        let quit: *mut Object = menu_item("Quit", sel!(terminate:), "q");
        let _: () = msg_send![app_menu, addItem: quit];
        let _: () = msg_send![quit, release];
        let _: () = msg_send![app_item, setSubmenu: app_menu];
        let _: () = msg_send![app_menu, release];
        let _: () = msg_send![main_menu, addItem: app_item];
        let _: () = msg_send![app_item, release];

        // Window menu — Minimize, Close.
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
        // Let AppKit manage the standard Window-menu behaviors.
        let _: () = msg_send![app, setWindowsMenu: win_menu];
        let _: () = msg_send![win_menu, release];
        let _: () = msg_send![main_menu, addItem: win_item];
        let _: () = msg_send![win_item, release];

        let _: () = msg_send![app, setMainMenu: main_menu];
        let _: () = msg_send![main_menu, release];
    }
}

// assemble_host_html
// Inputs: template (must contain CSS + JS placeholders), css and js bodies.
// Output: assembled HTML string ready for the webview.
// Errors: asserts both placeholders are present.
fn assemble_host_html(template: &str, css: &str, js: &str) -> String {
    assert!(template.contains("__HOST_CSS__"), "template missing CSS marker");
    assert!(template.contains("__HOST_JS__"), "template missing JS marker");
    template
        .replace("__HOST_CSS__", css)
        .replace("__HOST_JS__", js)
}

// assemble_present_html
// Inputs: the presentation template (must contain the present CSS + JS
// placeholders) plus the css and js bodies.
// Output: assembled HTML for the presentation webview.
// Errors: asserts both placeholders are present.
fn assemble_present_html(template: &str, css: &str, js: &str) -> String {
    assert!(template.contains("__PRESENT_CSS__"), "present template missing CSS marker");
    assert!(template.contains("__PRESENT_JS__"), "present template missing JS marker");
    template
        .replace("__PRESENT_CSS__", css)
        .replace("__PRESENT_JS__", js)
}

// build_presentation
// Inputs: the event-loop target (only available inside the run closure), an
// event-loop proxy for posting PresentIpcReceived, and the mpsc sender the
// presentation webview's IPC handler pushes decoded controls onto.
// Output: the fullscreen Window + its WebView, or a build error.
// Dataflow: build a borderless-fullscreen window, then a webview whose IPC
// handler decodes each body into a PresentInbound, forwards it on the channel,
// and wakes the loop. The caller keeps the Window alive and wraps the WebView
// in a WebviewSender.
fn build_presentation(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    present_tx: std::sync::mpsc::Sender<PresentInbound>,
) -> Result<(Window, WebView), Box<dyn std::error::Error>> {
    let window = WindowBuilder::new()
        .with_title("carousel — presenting")
        .with_fullscreen(Some(Fullscreen::Borderless(None)))
        .build(target)?;
    let html: String = assemble_present_html(PRESENT_HTML_TEMPLATE, PRESENT_CSS, PRESENT_JS);
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

// main
// Inputs: none.
// Output: process exit Result.
// Errors: window or webview construction failures bubble up.
// Dataflow:
//   1. Build Tao event loop, proxy, and an mpsc channel for IPC.
//   2. Build the Wry WebView whose IPC handler decodes JSON into
//      IpcMessage, pushes onto the channel, and posts IpcReceived.
//   3. Construct ApplicationCore around a WebviewSender (owns the
//      webview) plus a clone of the proxy for scheduling FlushPatches.
//   4. Run the event loop:
//      - IpcReceived  → drain channel, handle each message
//      - FlushPatches → drain coalesced patches, send Patch::Batch
//      - CloseRequested → exit
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    info!("starting carousel");

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let proxy_for_app = proxy.clone();
    let (ipc_tx, ipc_rx) = std::sync::mpsc::channel::<IpcMessage>();

    let window = WindowBuilder::new()
        .with_title("carousel")
        .with_inner_size(tao::dpi::LogicalSize::new(1400.0, 900.0))
        .build(&event_loop)?;

    let html: String = assemble_host_html(HOST_HTML_TEMPLATE, HOST_CSS, HOST_JS);
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

    let schedule_flush: Box<dyn Fn()> = {
        let p = proxy_for_app.clone();
        Box::new(move || {
            if p.send_event(UserEvent::FlushPatches).is_err() {
                error!("could not schedule FlushPatches; proxy closed");
            }
        })
    };

    // Presentation mode: a dedicated inbound channel for the presentation
    // webview's controls, plus two wakes the app uses to ask the event loop to
    // build / tear down the fullscreen window (window creation needs the
    // EventLoopWindowTarget, only reachable inside the run closure).
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

    // Bundle I/O: spawn the worker thread, wire its responses into the
    // event loop. The worker calls `io_wake` after every response so the
    // main thread knows to drain its receiver.
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

    let mut app: ApplicationCore = ApplicationCore::new(
        WebviewSender::new(webview),
        schedule_flush,
        io_thread,
        request_present_open,
        request_present_close,
    );

    // Install the standard Mac main menu (Quit / Minimize / Close). This
    // also gives wry's keyDown forwarding a valid performKeyEquivalent:
    // target, so unmapped keys no longer null-deref a missing main menu.
    #[cfg(target_os = "macos")]
    install_main_menu();

    // Presentation-window lifetime holders. The presentation WebView is owned
    // by the app's session (via its WebviewSender); the OS Window is held here
    // and dropped only after the session (and its webview) on close.
    let editor_window_id = window.id();
    let mut present_window: Option<Window> = None;
    let proxy_present = proxy_for_app.clone();

    info!("event loop running");
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::IpcReceived) => {
                while let Ok(msg) = ipc_rx.try_recv() {
                    if let Err(e) = app.handle_ipc(msg) {
                        error!("handle_ipc failed: {}", e);
                    }
                }
            }
            Event::UserEvent(UserEvent::FlushPatches) => {
                if let Err(e) = app.flush_patches() {
                    error!("flush_patches failed: {}", e);
                }
            }
            Event::UserEvent(UserEvent::IoResponse) => {
                while let Ok(resp) = io_rx.try_recv() {
                    if let Err(e) = app.handle_io_response(resp) {
                        error!("handle_io_response failed: {}", e);
                    }
                }
            }
            Event::UserEvent(UserEvent::OpenPresentation) => {
                if present_window.is_some() {
                    warn!("OpenPresentation ignored; already presenting");
                } else {
                    match build_presentation(target, proxy_present.clone(), present_tx.clone()) {
                        Ok((win, wv)) => {
                            app.begin_presentation(WebviewSender::new(wv));
                            present_window = Some(win);
                        }
                        Err(e) => error!("failed to build presentation window: {}", e),
                    }
                }
            }
            Event::UserEvent(UserEvent::PresentIpcReceived) => {
                while let Ok(ctrl) = present_rx.try_recv() {
                    if let Err(e) = app.handle_present_control(ctrl) {
                        error!("handle_present_control failed: {}", e);
                    }
                }
            }
            Event::UserEvent(UserEvent::ClosePresentation) => {
                // Drop the session (and its webview) first, then the window.
                app.end_presentation();
                present_window = None;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } => {
                if Some(window_id) == present_window.as_ref().map(|w| w.id()) {
                    info!("presentation window closed; ending presentation");
                    app.end_presentation();
                    present_window = None;
                } else if window_id == editor_window_id {
                    info!("close requested; exiting");
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
