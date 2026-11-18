//! Port of `packages/shared/src/socketFrameParts.ts` (t3code).
//!
//! Clients that connect with `?frameParts=1` accept large string frames split
//! into ~48 KiB parts; every part but the last is prefixed with U+001E.

pub const SOCKET_FRAME_PARTS_QUERY_PARAM: &str = "frameParts";
pub const SOCKET_FRAME_PART_BYTES: usize = 48 * 1024;
const PART_PREFIX: char = '\u{1e}';

/// Splits on the UTF-8 size the frame has on the wire, never inside a code point.
/// Byte-for-byte equivalent to the TypeScript `splitSocketFrame`.
pub fn split_socket_frame(frame: &str, part_bytes: usize) -> Vec<String> {
    if fits_whole(frame, part_bytes) {
        return vec![frame.to_owned()];
    }
    // Each part but the last holds as many whole characters as fit after the
    // prefix; cut points are found by jumping ahead and backing up to a
    // character boundary instead of walking every character.
    let budget = part_bytes.saturating_sub(PART_PREFIX.len_utf8());
    let mut parts = Vec::with_capacity(frame.len() / budget.max(1) + 1);
    let mut start = 0usize;
    while frame.len() - start > budget {
        let mut end = start + budget;
        while !frame.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            // A character larger than a part (only with tiny parts).
            return split_by_chars(frame, part_bytes);
        }
        let mut part = String::with_capacity(end - start + PART_PREFIX.len_utf8());
        part.push(PART_PREFIX);
        part.push_str(&frame[start..end]);
        parts.push(part);
        start = end;
    }
    parts.push(frame[start..].to_owned());
    parts
}

/// The splitting loop as ported, character by character.
fn split_by_chars(frame: &str, part_bytes: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut bytes = PART_PREFIX.len_utf8();
    for (index, ch) in frame.char_indices() {
        let size = ch.len_utf8();
        if bytes + size > part_bytes {
            let mut part = String::with_capacity(index - start + 1);
            part.push(PART_PREFIX);
            part.push_str(&frame[start..index]);
            parts.push(part);
            start = index;
            bytes = PART_PREFIX.len_utf8();
        }
        bytes += size;
    }
    parts.push(frame[start..].to_owned());
    parts
}

/// `split_socket_frame` for a frame the caller owns: one that fits is passed
/// on as it is, without a copy.
pub fn split_socket_frame_owned(frame: String, part_bytes: usize) -> Vec<String> {
    if fits_whole(&frame, part_bytes) {
        return vec![frame];
    }
    split_socket_frame(&frame, part_bytes)
}

/// Whether the frame goes out unsplit. A frame whose UTF-8 bytes plus the
/// part prefix fit is never split by the loop below; past that, the TS fast
/// path (UTF-16 length * 3, UTF-8 spending at most 3 bytes per UTF-16 code
/// unit) decides, so the result is the same as TypeScript's.
fn fits_whole(frame: &str, part_bytes: usize) -> bool {
    frame.len() + PART_PREFIX.len_utf8() <= part_bytes || frame.chars().map(char::len_utf16).sum::<usize>() * 3 <= part_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutting_at_boundaries_splits_exactly_like_the_ported_loop() {
        let alphabet = ['a', 'é', '€', '😀', '\u{1e}', '"'];
        let mut seed = 7u64;
        for len in [0usize, 1, 5, 60, 200, 1000] {
            for _ in 0..20 {
                let frame: String = (0..len).map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    alphabet[(seed >> 33) as usize % alphabet.len()]
                }).collect();
                for part_bytes in [5usize, 6, 7, 9, 16, 64, 333] {
                    if fits_whole(&frame, part_bytes) {
                        continue;
                    }
                    assert_eq!(split_socket_frame(&frame, part_bytes), split_by_chars(&frame, part_bytes), "{frame:?} at {part_bytes}");
                }
            }
        }
    }

    #[test]
    fn small_frames_are_untouched() {
        assert_eq!(split_socket_frame("{}", 16), vec!["{}".to_owned()]);
    }

    #[test]
    fn parts_rejoin_and_respect_budget() {
        let frame: String = "aé😀".repeat(1000);
        let parts = split_socket_frame(&frame, 64);
        assert!(parts.len() > 1);
        let (last, init) = parts.split_last().unwrap();
        let mut joined = String::new();
        for p in init {
            assert!(p.starts_with(PART_PREFIX));
            assert!(p.len() <= 64, "part too big: {}", p.len());
            joined.push_str(&p[PART_PREFIX.len_utf8()..]);
        }
        assert!(!last.starts_with(PART_PREFIX));
        joined.push_str(last);
        assert_eq!(joined, frame);
    }
}
