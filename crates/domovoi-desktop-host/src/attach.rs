//! A picture made ready to send with a message: told apart by its first
//! bytes, kept under the size a message takes, and written as a data URL (what
//! the core's `AddAttachment` carries). The desktop clients read the file or
//! the clipboard themselves and hand the bytes here.

use base64::Engine as _;

/// The most a picture may weigh.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;

/// A picture as `AddAttachment` takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub data_url: String,
}

/// The format of encoded picture bytes that a message can carry, from their
/// first bytes: (extension, MIME type).
pub fn sniff(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("png", "image/png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("gif", "image/gif"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

/// The attachment for a file's `bytes`, called `name`. The reason, for the
/// user, when it is not a picture a message can carry or is too large.
pub fn from_bytes(name: &str, bytes: &[u8]) -> Result<Attachment, String> {
    let Some((_, mime_type)) = sniff(bytes) else {
        return Err(format!("{name} can't be attached: only PNG, JPEG, GIF and WebP pictures can."));
    };
    if bytes.len() > MAX_BYTES {
        return Err(format!("{name} is larger than 20 MB."));
    }
    let data_url = format!("data:{mime_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    Ok(Attachment { name: name.to_owned(), mime_type: mime_type.to_owned(), size_bytes: bytes.len() as u64, data_url })
}

/// The attachment for the file at `path`.
pub fn from_file(path: &std::path::Path) -> Result<Attachment, String> {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("picture");
    let bytes = std::fs::read(path).map_err(|error| format!("{name} could not be read: {error}"))?;
    from_bytes(name, &bytes)
}

/// The attachment for a bitmap (a picture pasted from the clipboard): `rgba`
/// is `width` × `height` pixels of four bytes each. It is sent as a PNG.
pub fn from_rgba(name: &str, width: usize, height: usize, rgba: &[u8]) -> Result<Attachment, String> {
    let failed = |error: &dyn std::fmt::Display| format!("The pasted picture could not be encoded: {error}");
    let (w, h) = (u32::try_from(width).map_err(|e| failed(&e))?, u32::try_from(height).map_err(|e| failed(&e))?);
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| failed(&e))?;
        writer.write_image_data(rgba).map_err(|e| failed(&e))?;
    }
    from_bytes(name, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_told_by_its_bytes_not_its_name() {
        let gif = b"GIF89a\x01\x00\x01\x00";
        let attachment = from_bytes("notes.txt", gif).expect("a GIF, whatever it is called");
        assert_eq!((attachment.mime_type.as_str(), attachment.size_bytes), ("image/gif", gif.len() as u64));
        assert!(attachment.data_url.starts_with("data:image/gif;base64,R0lGODlh"));
        let refused = from_bytes("picture.png", b"just text").expect_err("text is not a picture");
        assert!(refused.contains("picture.png"), "{refused}");
    }

    #[test]
    fn a_pasted_bitmap_goes_as_a_png() {
        let attachment = from_rgba("pasted-image.png", 2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).expect("two pixels encode");
        assert_eq!(attachment.mime_type, "image/png");
        assert!(from_rgba("pasted-image.png", 2, 2, &[0; 4]).is_err(), "too few bytes for the size");
    }
}
