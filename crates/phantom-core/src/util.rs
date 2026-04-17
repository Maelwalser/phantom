//! Shared utilities used across the Phantom workspace.

/// Returns `true` if `buf` contains null bytes (in the first 8 000 bytes,
/// matching git's `buffer_is_binary` heuristic) or is not valid UTF-8.
///
/// Such content cannot be safely text-merged by line-based algorithms that
/// require `&str` input.
#[must_use]
pub fn is_binary_or_non_utf8(buf: &[u8]) -> bool {
    let check_len = buf.len().min(8000);
    buf[..check_len].contains(&0) || std::str::from_utf8(buf).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_null_bytes_as_binary() {
        let buf = b"hello\0world";
        assert!(is_binary_or_non_utf8(buf));
    }

    #[test]
    fn accepts_plain_text() {
        let buf = b"fn main() {}";
        assert!(!is_binary_or_non_utf8(buf));
    }

    #[test]
    fn detects_invalid_utf8() {
        let buf = &[0xff, 0xfe, 0xfd];
        assert!(is_binary_or_non_utf8(buf));
    }

    #[test]
    fn empty_buffer_is_text() {
        assert!(!is_binary_or_non_utf8(b""));
    }

    #[test]
    fn only_scans_first_8000_bytes_for_null() {
        let mut buf = vec![b'a'; 8000];
        buf.push(0);
        assert!(!is_binary_or_non_utf8(&buf));
    }
}
