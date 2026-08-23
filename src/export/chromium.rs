use crate::config;
use crate::export::pdf::PageRect;
use base64::Engine;
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::types::PrintToPdfOptions;
use headless_chrome::{Browser, LaunchOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(target_os = "macos")]
pub fn system_chrome_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
    ]
}

#[cfg(target_os = "windows")]
pub fn system_chrome_candidates() -> Vec<PathBuf> {
    let pf: String = std::env::var("PROGRAMFILES").unwrap_or_else(|_| "C:\\Program Files".into());
    let pf86: String =
        std::env::var("PROGRAMFILES(X86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
    vec![
        PathBuf::from(&pf).join("Google\\Chrome\\Application\\chrome.exe"),
        PathBuf::from(&pf86).join("Google\\Chrome\\Application\\chrome.exe"),
        PathBuf::from(&pf86).join("Microsoft\\Edge\\Application\\msedge.exe"),
    ]
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn system_chrome_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/bin/google-chrome"),
        PathBuf::from("/usr/bin/chromium"),
        PathBuf::from("/usr/bin/chromium-browser"),
        PathBuf::from("/usr/bin/microsoft-edge"),
    ]
}

pub fn is_valid_chrome(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }

    let child = Command::new(path).arg("--version").spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return false,
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}

#[cfg(target_os = "macos")]
pub fn normalize_chrome_path(picked: PathBuf) -> PathBuf {
    if picked.extension().and_then(|e| e.to_str()) != Some("app") {
        return picked;
    }
    let macos: PathBuf = picked.join("Contents").join("MacOS");
    if let Some(stem) = picked.file_stem() {
        let cand: PathBuf = macos.join(stem);
        if cand.is_file() {
            return cand;
        }
    }
    if let Ok(entries) = std::fs::read_dir(&macos) {
        for entry in entries.flatten() {
            let p: PathBuf = entry.path();
            if p.is_file() {
                return p;
            }
        }
    }
    picked
}

#[cfg(not(target_os = "macos"))]
pub fn normalize_chrome_path(picked: PathBuf) -> PathBuf {
    picked
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Found(PathBuf),
    NeedsUser,
}

pub fn resolve_from_config_or_system() -> Resolved {
    let mut cfg: config::Config = config::load();
    if let Some(p) = cfg.chrome_path.clone()
        && is_valid_chrome(&p)
    {
        return Resolved::Found(p);
    }
    for cand in system_chrome_candidates() {
        if is_valid_chrome(&cand) {
            cfg.chrome_path = Some(cand.clone());
            let _ = config::save(&cfg);
            return Resolved::Found(cand);
        }
    }
    Resolved::NeedsUser
}

#[derive(Debug)]
pub enum RenderError {
    Launch(String),
    Navigate(String),
    Screenshot(String),
    Print(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Launch(m) => write!(f, "chrome launch failed: {m}"),
            RenderError::Navigate(m) => write!(f, "page load failed: {m}"),
            RenderError::Screenshot(m) => write!(f, "screenshot failed: {m}"),
            RenderError::Print(m) => write!(f, "print failed: {m}"),
        }
    }
}
impl std::error::Error for RenderError {}

fn pdf_print_options() -> PrintToPdfOptions {
    PrintToPdfOptions {
        display_header_footer: Some(false),
        print_background: Some(true),
        margin_top: Some(0.0),
        margin_bottom: Some(0.0),
        margin_left: Some(0.0),
        margin_right: Some(0.0),
        prefer_css_page_size: Some(true),
        ..Default::default()
    }
}

fn splice_raster_page(tab: &headless_chrome::Tab, page: &PageRect) -> Result<(), RenderError> {
    let selector: String = format!("[data-raster-page=\"{}\"]", page.index);
    let element = tab
        .find_element(&selector)
        .map_err(|e| RenderError::Screenshot(e.to_string()))?;
    let png = element
        .capture_screenshot(CaptureScreenshotFormatOption::Png)
        .map_err(|e| RenderError::Screenshot(e.to_string()))?;
    let b64: String = base64::engine::general_purpose::STANDARD.encode(&png);
    let js: String = format!(
        "(function(){{var n=document.querySelector('{}');\
if(n){{n.innerHTML='<img style=\"width:100%;height:100%;display:block\" \
src=\"data:image/png;base64,{}\">';}}}})()",
        selector, b64
    );
    tab.evaluate(&js, false)
        .map_err(|e| RenderError::Screenshot(e.to_string()))?;
    Ok(())
}

pub fn render_pdf(
    chrome_path: &Path,
    print_html: &str,
    raster_pages: &[PageRect],
) -> Result<Vec<u8>, RenderError> {
    assert!(!print_html.is_empty(), "render_pdf: empty print html");
    let opts: LaunchOptions = LaunchOptions::default_builder()
        .path(Some(chrome_path.to_path_buf()))
        .headless(true)
        .build()
        .map_err(|e| RenderError::Launch(e.to_string()))?;
    let browser = Browser::new(opts).map_err(|e| RenderError::Launch(e.to_string()))?;
    let tab = browser
        .new_tab()
        .map_err(|e| RenderError::Launch(e.to_string()))?;

    let data_url: String = format!(
        "data:text/html;charset=utf-8;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(print_html.as_bytes())
    );
    tab.navigate_to(&data_url)
        .map_err(|e| RenderError::Navigate(e.to_string()))?;
    tab.wait_until_navigated()
        .map_err(|e| RenderError::Navigate(e.to_string()))?;

    for page in raster_pages {
        splice_raster_page(&tab, page)?;
    }

    let pdf = tab
        .print_to_pdf(Some(pdf_print_options()))
        .map_err(|e| RenderError::Print(e.to_string()))?;
    Ok(pdf)
}

const CHROMIUM_REVISION: &str = "1095492";
const CHROMIUM_SNAPSHOT_HOST: &str = "https://storage.googleapis.com";

fn chromium_platform_dir() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "Mac_Arm"
    }
    #[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
    {
        "Mac"
    }
    #[cfg(target_os = "windows")]
    {
        "Win_x64"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "Linux_x64"
    }
}

fn chromium_archive_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "chrome-mac"
    }
    #[cfg(target_os = "windows")]
    {
        "chrome-win"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "chrome-linux"
    }
}

fn chromium_binary_subpath() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("Chromium.app")
            .join("Contents")
            .join("MacOS")
            .join("Chromium")
    }
    #[cfg(target_os = "windows")]
    {
        PathBuf::from("chrome.exe")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        PathBuf::from("chrome")
    }
}

pub fn download_chromium(
    progress: &dyn Fn(u64, Option<u64>),
) -> Result<(PathBuf, String), RenderError> {
    use std::io::Read;
    let rev_dir: PathBuf = config::app_data_dir()
        .join("chromium")
        .join(CHROMIUM_REVISION);
    std::fs::create_dir_all(&rev_dir).map_err(|e| RenderError::Launch(e.to_string()))?;

    let url: String = format!(
        "{}/chromium-browser-snapshots/{}/{}/{}.zip",
        CHROMIUM_SNAPSHOT_HOST,
        chromium_platform_dir(),
        CHROMIUM_REVISION,
        chromium_archive_name()
    );
    progress(0, None);
    let mut resp = ureq::get(&url)
        .call()
        .map_err(|e| RenderError::Launch(e.to_string()))?;
    let total: Option<u64> = resp.body().content_length();
    let mut reader = resp.body_mut().as_reader();
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk: [u8; 65536] = [0u8; 65536];
    loop {
        let n: usize = reader
            .read(&mut chunk)
            .map_err(|e| RenderError::Launch(e.to_string()))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        progress(bytes.len() as u64, total);
    }
    drop(reader);

    extract_zip(&bytes, &rev_dir)?;
    let binary: PathBuf = rev_dir
        .join(chromium_archive_name())
        .join(chromium_binary_subpath());
    ensure_executable(&binary);
    if !binary.exists() {
        return Err(RenderError::Launch(format!(
            "chromium binary missing after extract: {binary:?}"
        )));
    }
    Ok((binary, CHROMIUM_REVISION.to_string()))
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), RenderError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| RenderError::Launch(e.to_string()))?;
    archive
        .extract(dest)
        .map_err(|e| RenderError::Launch(e.to_string()))
}

#[cfg(unix)]
fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_nonempty_and_absolute() {
        let c = system_chrome_candidates();
        assert!(!c.is_empty());
        for p in &c {
            assert!(p.is_absolute(), "not absolute: {p:?}");
        }
    }

    #[test]
    fn invalid_path_is_rejected() {
        assert!(!is_valid_chrome(std::path::Path::new(
            "/no/such/chrome-xyz"
        )));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn normalize_resolves_app_bundle_to_inner_binary() {
        let base = std::env::temp_dir().join(format!("carousel-norm-{}", std::process::id()));
        let macos = base.join("Foo.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();

        std::fs::write(macos.join("Helper"), b"x").unwrap();
        let bin = macos.join("Foo");
        std::fs::write(&bin, b"x").unwrap();

        assert_eq!(normalize_chrome_path(base.join("Foo.app")), bin);

        let plain = base.join("plain-chrome");
        assert_eq!(normalize_chrome_path(plain.clone()), plain);

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    #[ignore]
    fn render_pdf_smoke() {
        use crate::deck::Deck;
        use crate::export::pdf::{build_pdf_print_html, raster_page_rects};
        let chrome = match resolve_from_config_or_system() {
            Resolved::Found(p) => p,
            Resolved::NeedsUser => return,
        };
        let deck = Deck::sample();
        let html = build_pdf_print_html(&deck);
        let rects = raster_page_rects(&deck);
        let pdf = render_pdf(&chrome, &html, &rects).expect("render");
        assert!(pdf.starts_with(b"%PDF"), "not a PDF");
        assert!(pdf.len() > 1000);
    }

    #[test]
    #[ignore]
    fn download_chromium_smoke() {
        let (path, rev) = download_chromium(&|_recv, _total| {}).expect("download");
        assert!(path.exists());
        assert!(!rev.is_empty());
        assert!(is_valid_chrome(&path));
    }
}
