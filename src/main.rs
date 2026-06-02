mod app;
mod bundle;
mod commands;
mod deck;
mod error;
mod html;
mod ipc;

use app::ApplicationCore;
use bundle::{IoResponse, IoThread};
use ipc::IpcMessage;
use ipc::bridge::WebviewSender;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use tracing::{error, info};
use wry::WebViewBuilder;

const HOST_HTML_TEMPLATE: &str = include_str!("../assets/host.html");
const HOST_CSS: &str = include_str!("../assets/host.css");
const HOST_JS: &str = include_str!("../assets/host.js");

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

    let mut app: ApplicationCore =
        ApplicationCore::new(WebviewSender::new(webview), schedule_flush, io_thread);

    info!("event loop running");
    event_loop.run(move |event, _, control_flow| {
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
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                info!("close requested; exiting");
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
