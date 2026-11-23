//! What can be done with a transcript image: copy it, save it, or open it in
//! the system's viewer. All work from the original bytes the host fetched
//! (an `image::Handle::Bytes`), so nothing is downloaded again.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc;

use iced::widget::image;

/// The encoded bytes behind a handle the host made from a download.
pub fn image_bytes(handle: &image::Handle) -> Option<&[u8]> {
    match handle {
        image::Handle::Bytes(_, bytes) => Some(bytes.as_ref()),
        _ => None,
    }
}

/// The format of encoded image bytes, from their first bytes: (extension, MIME type).
pub fn sniff(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("png", "image/png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else if bytes.starts_with(b"BM") {
        Some(("bmp", "image/bmp"))
    } else {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
        let head = head.trim_start();
        (head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg"))).then_some(("svg", "image/svg+xml"))
    }
}

/// A file name for saving an image called `name`: its last path part, with
/// the extension its bytes call for when it has none (or a wrong one).
pub fn file_name(name: &str, bytes: &[u8]) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let base: String = base.chars().map(|c| if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect();
    let base = if base.is_empty() || base == "." || base == ".." { "image".to_owned() } else { base };
    let Some((ext, _)) = sniff(bytes) else { return base };
    let (stem, current) = match base.rsplit_once('.') {
        Some((stem, current)) if !stem.is_empty() => (stem, current.to_ascii_lowercase()),
        _ => (base.as_str(), String::new()),
    };
    let same = current == ext || (ext == "jpg" && current == "jpeg");
    if same { base.clone() } else { format!("{stem}.{ext}") }
}

// ── Clipboard ─────────────────────────────────────────────────────────

enum Job {
    Text(String),
    Image { width: usize, height: usize, rgba: Vec<u8>, done: tokio::sync::oneshot::Sender<Result<(), String>> },
}

/// One clipboard for the whole run, on its own thread. On Linux what was
/// copied is served by the process that copied it, and only while its
/// `arboard::Clipboard` lives, so a clipboard made and dropped per copy loses it.
fn clipboard() -> &'static mpsc::Sender<Job> {
    static CLIPBOARD: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    CLIPBOARD.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new().name("clipboard".into()).spawn(move || {
            let mut board = arboard::Clipboard::new().ok();
            for job in rx {
                if board.is_none() {
                    board = arboard::Clipboard::new().ok();
                }
                match job {
                    Job::Text(text) => {
                        if let Some(b) = board.as_mut() {
                            let _ = b.set_text(text);
                        }
                    }
                    Job::Image { width, height, rgba, done } => {
                        let result = match board.as_mut() {
                            Some(b) => b
                                .set_image(arboard::ImageData { width, height, bytes: Cow::Owned(rgba) })
                                .map_err(|e| format!("Couldn’t copy the image: {e}")),
                            None => Err("The clipboard isn’t available.".to_owned()),
                        };
                        let _ = done.send(result);
                    }
                }
            }
        });
        if let Err(e) = spawned {
            tracing::warn!(%e, "could not start the clipboard thread");
        }
        tx
    })
}

pub fn copy_text(text: String) {
    let _ = clipboard().send(Job::Text(text));
}

/// Decodes the image and puts its pixels on the clipboard.
pub async fn copy_image(bytes: Vec<u8>) -> Result<(), String> {
    let decoded = tokio::task::spawn_blocking(move || ::image::load_from_memory(&bytes).map(|i| i.to_rgba8()))
        .await
        .map_err(|_| "Couldn’t copy the image.".to_owned())?
        .map_err(|e| format!("Couldn’t read the image: {e}"))?;
    let (width, height) = (decoded.width() as usize, decoded.height() as usize);
    let (done, result) = tokio::sync::oneshot::channel();
    clipboard()
        .send(Job::Image { width, height, rgba: decoded.into_raw(), done })
        .map_err(|_| "The clipboard isn’t available.".to_owned())?;
    result.await.unwrap_or_else(|_| Err("The clipboard isn’t available.".to_owned()))
}

/// Asks where to save the image and writes its original bytes there. `None`
/// when the dialog was cancelled.
pub async fn save_as(bytes: Vec<u8>, name: String) -> Result<Option<PathBuf>, String> {
    let suggested = file_name(&name, &bytes);
    let mut dialog = rfd::AsyncFileDialog::new().set_file_name(&suggested);
    if let Some((ext, _)) = sniff(&bytes) {
        dialog = dialog.add_filter("Image", &[ext]);
    }
    let Some(target) = dialog.save_file().await else { return Ok(None) };
    let path = target.path().to_path_buf();
    tokio::fs::write(&path, &bytes).await.map_err(|e| format!("Couldn’t save the image: {e}"))?;
    Ok(Some(path))
}

/// Writes the image to a temporary file and opens it in the system's viewer
/// (which can share, print or edit it).
pub async fn open_externally(bytes: Vec<u8>, name: String) -> Result<(), String> {
    let dir = std::env::temp_dir().join("domovoi-images");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("Couldn’t open the image: {e}"))?;
    let path = dir.join(file_name(&name, &bytes));
    tokio::fs::write(&path, &bytes).await.map_err(|e| format!("Couldn’t open the image: {e}"))?;
    tokio::task::spawn_blocking(move || open::that(&path))
        .await
        .map_err(|_| "Couldn’t open the image.".to_owned())?
        .map_err(|e| format!("Couldn’t open the image: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10];

    #[test]
    fn formats_are_told_by_their_first_bytes() {
        assert_eq!(sniff(PNG), Some(("png", "image/png")));
        assert_eq!(sniff(JPEG), Some(("jpg", "image/jpeg")));
        assert_eq!(sniff(b"GIF89a...."), Some(("gif", "image/gif")));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(("webp", "image/webp")));
        assert_eq!(sniff(b"  <svg xmlns=\"http://www.w3.org/2000/svg\"/>"), Some(("svg", "image/svg+xml")));
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn saved_names_keep_the_name_and_get_the_right_extension() {
        assert_eq!(file_name("screenshot.png", PNG), "screenshot.png");
        assert_eq!(file_name("photo.JPEG", JPEG), "photo.JPEG");
        assert_eq!(file_name("src/assets/hero", PNG), "hero.png");
        assert_eq!(file_name("C:\\shots\\wrong.jpg", PNG), "wrong.png");
        assert_eq!(file_name("", JPEG), "image.jpg");
        assert_eq!(file_name("what?.png", PNG), "what_.png");
        assert_eq!(file_name(".png", PNG), ".png.png");
        assert_eq!(file_name("notes", b"plain"), "notes");
    }

    #[test]
    fn only_downloaded_handles_have_bytes() {
        let handle = image::Handle::from_bytes(PNG.to_vec());
        assert_eq!(image_bytes(&handle), Some(PNG));
        assert_eq!(image_bytes(&image::Handle::from_rgba(1, 1, vec![0u8; 4])), None);
    }
}
