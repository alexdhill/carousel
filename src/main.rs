mod app;
mod bundle;
mod commands;
mod deck;
mod error;
mod export;
mod fonts;
mod html;
mod recents;
mod ipc;
mod present;

use app::ApplicationCore;
use bundle::{IoResponse, IoThread};
use deck::Deck;
use ipc::IpcMessage;
use ipc::bridge::WebviewSender;
use ipc::landing::{LandingData, LandingInbound, LandingRecent, LandingTemplate};
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
const LANDING_HTML_TEMPLATE: &str = include_str!("../assets/landing.html");
const LANDING_CSS: &str = include_str!("../assets/landing.css");
const LANDING_JS: &str = include_str!("../assets/landing.js");

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
    // PDF export. The app queued a print job and asked the event loop to build
    // the hidden print webview (OpenPdfPrint); that webview signals once its
    // content has loaded and is ready to print (PdfPrintReady).
    OpenPdfPrint,
    PdfPrintReady,
    // The headless print operation finished (carries success). Posted by the
    // print completion delegate so the loop can toast + tear the webview down.
    PdfPrintDone(bool),
    // The landing webview pushed a control (Ready / Open* / Cancel) onto its
    // channel; the main thread drains it and either sends the landing data,
    // builds the editor, or exits.
    LandingIpcReceived,
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
// Inputs: template (must contain CSS + crop-JS + snap-JS + style-props-JS +
// host-JS placeholders), css, host js, and the pure snap-, crop-, and
// style-props-engine js (injected before host js).
// Output: assembled HTML string ready for the webview.
// Errors: asserts all five placeholders are present.
fn assemble_host_html(template: &str, css: &str, js: &str, snap: &str, crop: &str,
        style_props: &str, preset_css: &str) -> String {
    assert!(template.contains("__HOST_CSS__"), "template missing CSS marker");
    assert!(template.contains("__HOST_JS__"), "template missing JS marker");
    assert!(template.contains("__SNAP_JS__"), "template missing snap JS marker");
    assert!(template.contains("__CROP_JS__"), "template missing crop JS marker");
    assert!(template.contains("__STYLE_PROPS_JS__"), "template missing style-props JS marker");
    assert!(template.contains("__PRESET_CSS_JS__"), "template missing preset-css JS marker");
    template
        .replace("__HOST_CSS__", css)
        .replace("__CROP_JS__", crop)
        .replace("__SNAP_JS__", snap)
        .replace("__STYLE_PROPS_JS__", style_props)
        .replace("__PRESET_CSS_JS__", preset_css)
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

// build_pdf_print_window
// Inputs: the event-loop target, a proxy for posting PdfPrintReady, and the
// print-HTML (one page per stage). Output: a small hidden-ish window + its
// webview loaded with the print-HTML. The webview's IPC handler posts
// PdfPrintReady once the page signals "print-ready" (window.onload), at which
// point the event loop triggers the print. The caller keeps both alive until
// printing finishes.
fn build_pdf_print_window(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
    html: String,
) -> Result<(Window, WebView), Box<dyn std::error::Error>> {
    let window = WindowBuilder::new()
        .with_title("carousel — exporting PDF")
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 360.0))
        .build(target)?;
    let webview = WebViewBuilder::new(&window)
        .with_html(html)
        .with_ipc_handler(move |request: wry::http::Request<String>| {
            if request.body() == "print-ready"
                && proxy.send_event(UserEvent::PdfPrintReady).is_err()
            {
                error!("event loop proxy closed; cannot dispatch PdfPrintReady");
            }
        })
        .build()?;
    Ok((window, webview))
}

// pdf_print_did_run (macOS)
// The NSPrintOperation completion selector. `contextInfo` is a leaked boxed
// EventLoopProxy; we reclaim it, post PdfPrintDone(success), and free it.
#[cfg(target_os = "macos")]
extern "C" fn pdf_print_did_run(
    _this: &objc::runtime::Object,
    _cmd: objc::runtime::Sel,
    _op: *mut objc::runtime::Object,
    success: objc::runtime::BOOL,
    ctx: *mut std::ffi::c_void,
) {
    if ctx.is_null() {
        return;
    }
    let proxy: Box<tao::event_loop::EventLoopProxy<UserEvent>> =
        unsafe { Box::from_raw(ctx as *mut tao::event_loop::EventLoopProxy<UserEvent>) };
    let ok: bool = success != objc::runtime::NO;
    if proxy.send_event(UserEvent::PdfPrintDone(ok)).is_err() {
        error!("event loop proxy closed; cannot dispatch PdfPrintDone");
    }
}

// pdf_print_delegate_class (macOS)
// Registers (once) a tiny NSObject subclass exposing the print completion
// selector, returning the class pointer.
#[cfg(target_os = "macos")]
fn pdf_print_delegate_class() -> *const objc::runtime::Class {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel, BOOL};
    use objc::{class, sel, sel_impl};
    static CLS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr: usize = *CLS.get_or_init(|| match ClassDecl::new("CarouselPdfPrintDelegate", class!(NSObject)) {
        Some(mut decl) => {
            unsafe {
                decl.add_method(
                    sel!(printOperationDidRun:success:contextInfo:),
                    pdf_print_did_run
                        as extern "C" fn(&Object, Sel, *mut Object, BOOL, *mut std::ffi::c_void),
                );
            }
            decl.register() as *const Class as usize
        }
        None => Class::get("CarouselPdfPrintDelegate")
            .map(|c| c as *const Class as usize)
            .unwrap_or(0),
    });
    ptr as *const Class
}

// start_pdf_print (macOS)
// Inputs: the print webview (already loaded), the destination path, and a proxy
// for the completion event. Output: side-effect; configures an NSPrintInfo as a
// save-to-PDF job (no panels) and runs the WKWebView's print operation
// ASYNCHRONOUSLY (runOperationModalForWindow…), so the main thread is never
// blocked. The completion delegate posts PdfPrintDone. All AppKit interop is
// confined here. The print paginates per the @page / page-break CSS.
#[cfg(target_os = "macos")]
fn start_pdf_print(
    webview: &WebView,
    dest: &std::path::Path,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use wry::WebViewExtMacOS;

    // AppKit NSString constants: the save-job disposition value and the
    // dictionary key for the output URL. (cocoa::base::id is *mut Object.)
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSPrintSaveJob: *mut Object;
        static NSPrintJobSavingURL: *mut Object;
    }

    let wk: *mut Object = webview.webview();
    let win: *mut Object = webview.ns_window();
    if wk.is_null() {
        let _ = proxy.send_event(UserEvent::PdfPrintDone(false));
        return;
    }
    unsafe {
        let responds: bool =
            msg_send![wk, respondsToSelector: sel!(printOperationWithPrintInfo:)];
        if !responds {
            error!("WKWebView lacks printOperationWithPrintInfo:");
            let _ = proxy.send_event(UserEvent::PdfPrintDone(false));
            return;
        }
        let path_ns: *mut Object = ns_string(&dest.to_string_lossy());
        let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: path_ns];

        let shared: *mut Object = msg_send![class!(NSPrintInfo), sharedPrintInfo];
        let info: *mut Object = msg_send![shared, copy];
        let () = msg_send![info, setJobDisposition: NSPrintSaveJob];
        let dict: *mut Object = msg_send![info, dictionary];
        let () = msg_send![dict, setObject: url forKey: NSPrintJobSavingURL];
        let () = msg_send![info, setTopMargin: 0.0f64];
        let () = msg_send![info, setBottomMargin: 0.0f64];
        let () = msg_send![info, setLeftMargin: 0.0f64];
        let () = msg_send![info, setRightMargin: 0.0f64];

        let op: *mut Object = msg_send![wk, printOperationWithPrintInfo: info];
        if op.is_null() {
            error!("printOperationWithPrintInfo: returned nil");
            let _ = proxy.send_event(UserEvent::PdfPrintDone(false));
            return;
        }
        let () = msg_send![op, setShowsPrintPanel: false];
        let () = msg_send![op, setShowsProgressPanel: false];

        // Run asynchronously: returns immediately, posting PdfPrintDone via the
        // delegate when the write finishes. The proxy is handed off as a leaked
        // box reclaimed in pdf_print_did_run.
        let delegate: *mut Object = msg_send![pdf_print_delegate_class(), new];
        let ctx: *mut std::ffi::c_void =
            Box::into_raw(Box::new(proxy)) as *mut std::ffi::c_void;
        let () = msg_send![op,
            runOperationModalForWindow: win
            delegate: delegate
            didRunSelector: sel!(printOperationDidRun:success:contextInfo:)
            contextInfo: ctx];
    }
}

// start_pdf_print (non-macOS): headless PDF is macOS-only for now.
#[cfg(not(target_os = "macos"))]
fn start_pdf_print(
    _webview: &WebView,
    _dest: &std::path::Path,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) {
    error!("headless PDF export is only supported on macOS");
    let _ = proxy.send_event(UserEvent::PdfPrintDone(false));
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
// assemble_landing_html
// Inputs: the landing template (CSS + JS placeholders) and the two bodies.
// Output: the assembled landing HTML. Errors: asserts both placeholders.
fn assemble_landing_html(template: &str, css: &str, js: &str) -> String {
    assert!(template.contains("__LANDING_CSS__"), "landing template missing CSS marker");
    assert!(template.contains("__LANDING_JS__"), "landing template missing JS marker");
    template.replace("__LANDING_CSS__", css).replace("__LANDING_JS__", js)
}

// build_landing
// Inputs: the event-loop target, a proxy, and the landing inbound channel.
// Output: the landing window + webview. The webview's IPC handler decodes each
// body into a LandingInbound, forwards it, and wakes LandingIpcReceived.
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

// build_editor
// Inputs: the event-loop target, a proxy, the editor IPC channel sender, the
// starting deck, and the app's flush/io/present/pdf wiring. Output: the editor
// window + a ready ApplicationCore. Moves the (formerly eager) editor
// construction so the landing window can build it lazily on Open.
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
    request_pdf_print: Box<dyn Fn()>,
) -> Result<(Window, ApplicationCore), Box<dyn std::error::Error>> {
    let window = WindowBuilder::new()
        .with_title("carousel")
        .with_inner_size(tao::dpi::LogicalSize::new(1400.0, 900.0))
        .build(target)?;
    let html: String = assemble_host_html(HOST_HTML_TEMPLATE, HOST_CSS, HOST_JS, SNAP_JS, CROP_JS,
        STYLE_PROPS_JS, PRESET_CSS_JS);
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
        request_pdf_print,
    );
    Ok((window, app))
}

// send_landing
// Inputs: the landing webview + the data payload. Output: side-effect; calls
// window.__landing.receive(<json>) in the landing webview.
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

// landing_data
// Output: the recents + template rows for the landing webview.
fn landing_data() -> LandingData {
    let recents: Vec<LandingRecent> = recents::load()
        .into_iter()
        .map(|r| LandingRecent { path: r.path, title: r.title, modified: r.modified })
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

// deck_for_open
// Inputs: an Open* landing control. Output: the starting deck plus, for a
// recent, the path to load asynchronously (the deck is a light placeholder
// swapped out when the load returns). None means "abort, stay on the landing"
// — used when the OpenDefault file dialog is cancelled.
fn deck_for_open(inbound: &LandingInbound) -> Option<(Deck, Option<PathBuf>)> {
    use deck::templates::{light_theme, new_deck, new_deck_all_layouts, theme_by_id};
    match inbound {
        LandingInbound::OpenTemplate { theme_id, .. } => {
            Some((new_deck_all_layouts(theme_by_id(theme_id)), None))
        }
        LandingInbound::OpenRecent { path } => {
            Some((new_deck(light_theme(), "title"), Some(PathBuf::from(path))))
        }
        // No selection: let the user pick an existing .slidedeck from disk.
        // Cancel -> None (stay on landing). The placeholder light deck is
        // swapped out when the load returns.
        LandingInbound::OpenDefault => {
            let path: PathBuf = rfd::FileDialog::new()
                .add_filter("Slide Deck", &["slidedeck"])
                .pick_file()?;
            Some((new_deck(light_theme(), "title"), Some(path)))
        }
        _ => None,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    info!("starting carousel");

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let proxy_for_app = proxy.clone();
    let (ipc_tx, ipc_rx) = std::sync::mpsc::channel::<IpcMessage>();

    // Landing window first. The editor is built lazily when the landing posts
    // an Open control (window + deck construction needs the
    // EventLoopWindowTarget, only reachable in the run closure). `ipc_tx` and
    // `proxy` are kept for build_editor.
    let (landing_tx, landing_rx) = std::sync::mpsc::channel::<LandingInbound>();
    let (landing_win, landing_wv) = build_landing(&event_loop, proxy.clone(), landing_tx)?;
    let mut landing_window: Option<Window> = Some(landing_win);
    let mut landing_webview: Option<WebView> = Some(landing_wv);

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
    let request_pdf_print: Box<dyn Fn()> = {
        let p = proxy_for_app.clone();
        Box::new(move || {
            if p.send_event(UserEvent::OpenPdfPrint).is_err() {
                error!("could not schedule OpenPdfPrint; proxy closed");
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

    // Editor ingredients, moved into build_editor on the first Open.
    let mut schedule_flush_opt: Option<Box<dyn Fn()>> = Some(schedule_flush);
    let mut io_thread_opt: Option<IoThread> = Some(io_thread);
    let mut request_present_open_opt: Option<Box<dyn Fn()>> = Some(request_present_open);
    let mut request_present_close_opt: Option<Box<dyn Fn()>> = Some(request_present_close);
    let mut request_pdf_print_opt: Option<Box<dyn Fn()>> = Some(request_pdf_print);
    let mut app: Option<ApplicationCore> = None;
    let mut editor_window: Option<Window> = None;

    // Install the standard Mac main menu (Quit / Minimize / Close). This
    // also gives wry's keyDown forwarding a valid performKeyEquivalent:
    // target, so unmapped keys no longer null-deref a missing main menu.
    #[cfg(target_os = "macos")]
    install_main_menu();

    // Presentation-window lifetime holders. The presentation WebView is owned
    // by the app's session (via its WebviewSender); the OS Window is held here
    // and dropped only after the session (and its webview) on close.
    let mut present_window: Option<Window> = None;
    // Holds the transient PDF-print window+webview (and its optional destination)
    // while it loads and prints; dropped once printing finishes.
    let mut print_session: Option<(Window, WebView, Option<std::path::PathBuf>)> = None;
    let proxy_present = proxy_for_app.clone();
    let proxy_print = proxy_for_app.clone();

    info!("event loop running");
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::IpcReceived) => {
                while let Ok(msg) = ipc_rx.try_recv() {
                    if let Some(app) = app.as_mut() {
                        if let Err(e) = app.handle_ipc(msg) {
                            error!("handle_ipc failed: {}", e);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::FlushPatches) => {
                if let Some(app) = app.as_mut() {
                    if let Err(e) = app.flush_patches() {
                        error!("flush_patches failed: {}", e);
                    }
                }
            }
            Event::UserEvent(UserEvent::IoResponse) => {
                while let Ok(resp) = io_rx.try_recv() {
                    if let Some(app) = app.as_mut() {
                        if let Err(e) = app.handle_io_response(resp) {
                            error!("handle_io_response failed: {}", e);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::OpenPresentation) => {
                if let Some(app) = app.as_mut() {
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
            }
            Event::UserEvent(UserEvent::PresentIpcReceived) => {
                while let Ok(ctrl) = present_rx.try_recv() {
                    if let Some(app) = app.as_mut() {
                        if let Err(e) = app.handle_present_control(ctrl) {
                            error!("handle_present_control failed: {}", e);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::ClosePresentation) => {
                // Drop the session (and its webview) first, then the window.
                if let Some(app) = app.as_mut() {
                    app.end_presentation();
                }
                present_window = None;
            }
            Event::UserEvent(UserEvent::OpenPdfPrint) => {
                if let Some(app) = app.as_mut() {
                    if let Some((html, dest)) = app.take_pending_pdf_print() {
                        // Drop any prior print session (its async job has finished
                        // by the time a new export is triggered).
                        print_session = None;
                        match build_pdf_print_window(target, proxy_print.clone(), html) {
                            Ok((win, wv)) => print_session = Some((win, wv, dest)),
                            Err(e) => error!("failed to build pdf print window: {}", e),
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::PdfPrintReady) => {
                match print_session.as_ref() {
                    // A: headless save-to-file, run ASYNC. Keep the session alive
                    // until PdfPrintDone so the webview outlives the print.
                    Some((_, wv, Some(path))) => {
                        start_pdf_print(wv, path, proxy_print.clone());
                    }
                    // B fallback: interactive dialog (user picks Save as PDF).
                    Some((_, wv, None)) => {
                        if let Err(e) = wv.print() {
                            error!("pdf print failed: {}", e);
                        }
                        print_session = None;
                    }
                    None => {}
                }
            }
            Event::UserEvent(UserEvent::PdfPrintDone(ok)) => {
                if let Some((win, wv, dest)) = print_session.take() {
                    if let Some(path) = &dest {
                        if let Some(app) = app.as_mut() {
                            app.notify_pdf_export(path, ok);
                        }
                    }
                    drop(win);
                    drop(wv);
                }
            }
            Event::UserEvent(UserEvent::LandingIpcReceived) => {
                while let Ok(inbound) = landing_rx.try_recv() {
                    match inbound {
                        LandingInbound::Ready => {
                            if let Some(wv) = landing_webview.as_ref() {
                                send_landing(wv, &landing_data());
                            }
                        }
                        LandingInbound::Cancel => {
                            if app.is_none() {
                                info!("landing: cancelled; exiting");
                                *control_flow = ControlFlow::Exit;
                            }
                        }
                        // Any Open* control: build the editor on the chosen deck,
                        // then drop the landing window. Ignored if already open.
                        open => {
                            // Resolve the deck first (OpenDefault may pop a file
                            // dialog the user can cancel) so we only consume the
                            // editor ingredients once we're committed to opening.
                            let chosen = if app.is_some() {
                                warn!("landing open ignored; editor already open");
                                None
                            } else {
                                deck_for_open(&open)
                            };
                            // Only consume the editor ingredients once a deck is
                            // committed (a cancelled dialog leaves them intact).
                            if let Some((deck, load)) = chosen {
                                if let (Some(sf), Some(io), Some(rpo), Some(rpc), Some(rpp)) = (
                                    schedule_flush_opt.take(),
                                    io_thread_opt.take(),
                                    request_present_open_opt.take(),
                                    request_present_close_opt.take(),
                                    request_pdf_print_opt.take(),
                                ) {
                                    match build_editor(
                                        target, proxy.clone(), ipc_tx.clone(), deck, sf, io, rpo, rpc, rpp,
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
                } else if Some(window_id) == print_session.as_ref().map(|(w, _, _)| w.id()) {
                    info!("pdf print window closed");
                    print_session = None;
                } else if Some(window_id) == editor_window.as_ref().map(|w| w.id()) {
                    info!("editor closed; exiting");
                    *control_flow = ControlFlow::Exit;
                } else if Some(window_id) == landing_window.as_ref().map(|w| w.id()) {
                    info!("landing closed; exiting");
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
