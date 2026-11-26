//! Large frames, sent packed.
//!
//! A thread's snapshot is megabytes of JSON that is mostly text, and a
//! phone reads it over a relay. A client that asks for it (`pack=1` on the
//! socket URL) is sent every frame of [`PACK_FROM`] bytes or more deflated:
//! such a frame starts with [`PACKED`], a character no JSON frame starts
//! with, and the rest is the deflated JSON in base64.
//!
//! Base64, because a frame crosses to the client core as text in every
//! shell: a packed frame costs a third more than its deflated bytes, and
//! needs nothing of the shells. The cost goes when the socket is read in
//! Rust.
//!
//! Measured on a real home of 1,027 threads (`docs/performance.md`): their
//! snapshots are 233 MB as JSON and 77 MB packed. A thread that is mostly
//! images (base64 inside the JSON) gains nothing: deflate takes a quarter off
//! such text and the base64 here puts it back.

use base64::Engine as _;
use std::borrow::Cow;
use std::io::{Read as _, Write as _};

/// What a packed frame starts with (U+001F).
pub const PACKED: char = '\u{1f}';

/// The size from which a frame is packed. Below it the frame is a few
/// packets either way.
pub const PACK_FROM: usize = 2048;

/// The deflate level. On the real home's threads level 1 leaves 99 MB at
/// 216 MB/s, level 3 leaves 77 MB at 112 MB/s, and level 6 leaves 75 MB at
/// 62 MB/s.
const LEVEL: u32 = 3;

/// The size from which packing a frame is worth a thread of its own: it
/// takes milliseconds, which a task shared with other connections must not.
pub const PACK_APART_FROM: usize = 64 * 1024;

/// The most a packed frame may unpack to: a frame that claims more is refused
/// before it is held in memory.
pub const UNPACKED_LIMIT: u64 = 256 * 1024 * 1024;

/// `frame` as it is sent to a client that asked for packed frames: packed if
/// it is large and packing makes it smaller, as it is otherwise.
pub fn pack(frame: String) -> String {
    if frame.len() < PACK_FROM {
        return frame;
    }
    let mut deflater = flate2::write::DeflateEncoder::new(Vec::with_capacity(frame.len() / 4), flate2::Compression::new(LEVEL));
    let deflated = match deflater.write_all(frame.as_bytes()).and_then(|()| deflater.finish()) {
        Ok(deflated) => deflated,
        Err(_) => return frame,
    };
    if deflated.len() / 3 * 4 + 8 >= frame.len() {
        return frame;
    }
    let mut packed = String::with_capacity(deflated.len() / 3 * 4 + 8);
    packed.push(PACKED);
    base64::engine::general_purpose::STANDARD_NO_PAD.encode_string(&deflated, &mut packed);
    packed
}

/// Why a packed frame could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// What follows [`PACKED`] is not base64 of deflated UTF-8.
    Malformed,
    /// It unpacks to more than [`UNPACKED_LIMIT`].
    TooLarge,
}

/// The frame `text` carries: itself, or what it packs.
pub fn unpack(text: &str) -> Result<Cow<'_, str>, Unreadable> {
    let Some(packed) = text.strip_prefix(PACKED) else { return Ok(Cow::Borrowed(text)) };
    let deflated = base64::engine::general_purpose::STANDARD_NO_PAD.decode(packed).map_err(|_| Unreadable::Malformed)?;
    let mut frame = String::with_capacity(deflated.len() * 4);
    let mut inflater = flate2::read::DeflateDecoder::new(deflated.as_slice()).take(UNPACKED_LIMIT + 1);
    inflater.read_to_string(&mut frame).map_err(|_| Unreadable::Malformed)?;
    if frame.len() as u64 > UNPACKED_LIMIT {
        return Err(Unreadable::TooLarge);
    }
    Ok(Cow::Owned(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_frame_is_packed_and_reads_back_and_a_small_one_is_sent_as_it_is() {
        let small = r#"{"_tag":"Pong"}"#.to_owned();
        assert_eq!(pack(small.clone()), small);
        assert_eq!(unpack(&small).unwrap(), small);

        let large = format!(r#"{{"_tag":"Chunk","requestId":"1","values":["{}"]}}"#, "wörds and more wörds, ".repeat(2000));
        let packed = pack(large.clone());
        assert!(packed.starts_with(PACKED) && packed.len() * 10 < large.len(), "{} -> {}", large.len(), packed.len());
        assert_eq!(unpack(&packed).unwrap(), large);
        assert!(matches!(unpack(&packed).unwrap(), Cow::Owned(_)));

        // What does not get smaller is not packed: random-looking text of the same length.
        let noise: String = (0..PACK_FROM * 2).map(|i| char::from(b'!' + ((i * 7919 + i / 3 * 104_729) % 90) as u8)).collect();
        assert!(!pack(noise.clone()).starts_with(PACKED) || pack(noise.clone()).len() < noise.len());
    }

    #[test]
    fn a_frame_that_is_not_what_it_says_is_refused() {
        assert_eq!(unpack("\u{1f}not base64!"), Err(Unreadable::Malformed));
        assert_eq!(unpack("\u{1f}AAAA"), Err(Unreadable::Malformed), "base64, but not deflated text");
    }
}
