// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `Content-Length` framing of the language server protocol.

use std::fmt;
use std::io::{self, BufRead, Read, Write};

/// Upper bounds a peer cannot exceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes in the header block, terminators included.
    pub max_header: usize,
    /// Bytes in the message body.
    pub max_body: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_header: 4 * 1024,
            max_body: 8 * 1024 * 1024,
        }
    }
}

/// Why a frame could not be read.
#[derive(Debug)]
pub enum FrameError {
    /// The stream ended before the first byte of a frame.
    Closed,
    /// The stream ended inside a frame.
    UnexpectedEof,
    HeaderLimit {
        limit: usize,
    },
    BodyLimit {
        limit: usize,
        claimed: usize,
    },
    MissingLength,
    BadLength(String),
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "the server closed the connection"),
            Self::UnexpectedEof => write!(f, "unexpected end of input inside a message"),
            Self::HeaderLimit { limit } => {
                write!(
                    f,
                    "message header exceeds the header limit of {limit} bytes"
                )
            }
            Self::BodyLimit { limit, claimed } => write!(
                f,
                "message body of {claimed} bytes exceeds the body limit of {limit} bytes"
            ),
            Self::MissingLength => write!(f, "message has no Content-Length header"),
            Self::BadLength(value) => {
                write!(f, "message has an invalid Content-Length value {value:?}")
            }
            Self::Io(e) => write!(f, "read failed: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Reads one frame and returns its body.
pub fn read_message<R: BufRead>(reader: &mut R, limits: Limits) -> Result<Vec<u8>, FrameError> {
    let length = read_header_length(reader, limits.max_header)?;
    if length > limits.max_body {
        return Err(FrameError::BodyLimit {
            limit: limits.max_body,
            claimed: length,
        });
    }
    let mut body = Vec::with_capacity(length);
    reader.take(length as u64).read_to_end(&mut body)?;
    if body.len() < length {
        return Err(FrameError::UnexpectedEof);
    }
    Ok(body)
}

fn read_header_length<R: BufRead>(reader: &mut R, max_header: usize) -> Result<usize, FrameError> {
    let mut used = 0;
    let mut length = None;
    loop {
        let mut line = Vec::new();
        // One byte past the room left, so an overlong line is detected, not truncated.
        let room = (max_header - used) as u64 + 1;
        let n = reader.by_ref().take(room).read_until(b'\n', &mut line)?;
        used += n;
        if used > max_header {
            return Err(FrameError::HeaderLimit { limit: max_header });
        }
        if n == 0 {
            return Err(if used == 0 {
                FrameError::Closed
            } else {
                FrameError::UnexpectedEof
            });
        }
        if !line.ends_with(b"\n") {
            return Err(FrameError::UnexpectedEof);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            return length.ok_or(FrameError::MissingLength);
        }
        if let Some((name, value)) = text.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let value = value.trim();
                length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| FrameError::BadLength(value.to_string()))?,
                );
            }
        }
    }
}

/// Writes `body` as one frame and flushes.
pub fn write_message<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    writer.write_all(&frame)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    fn read(bytes: &[u8], limits: Limits) -> Result<Vec<u8>, FrameError> {
        read_message(&mut Cursor::new(bytes.to_vec()), limits)
    }

    /// Hands out one byte per `read` call.
    struct OneByte(Cursor<Vec<u8>>);

    impl Read for OneByte {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let take = buf.len().min(1);
            self.0.read(&mut buf[..take])
        }
    }

    #[test]
    fn a_multibyte_body_round_trips_and_length_counts_bytes() {
        // Arrange
        let body = "héllo 世界".as_bytes();
        let mut wire = Vec::new();

        // Act
        write_message(&mut wire, body).unwrap();
        let back = read(&wire, Limits::default()).unwrap();

        // Assert
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        assert!(wire.starts_with(header.as_bytes()));
        assert_eq!(back, body);
    }

    #[test]
    fn the_body_limit_admits_the_cap_and_rejects_one_more() {
        let limits = Limits {
            max_header: 64,
            max_body: 8,
        };
        for (len, ok) in [(7, true), (8, true), (9, false)] {
            // Arrange
            let mut wire = format!("Content-Length: {len}\r\n\r\n").into_bytes();
            wire.extend(vec![b'x'; len]);

            // Act
            let got = read(&wire, limits);

            // Assert
            match (ok, got) {
                (true, Ok(body)) => assert_eq!(body.len(), len),
                (false, Err(e @ FrameError::BodyLimit { .. })) => {
                    assert!(e.to_string().contains("body limit"), "{e}");
                }
                (_, other) => panic!("length {len}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_header_limit_admits_the_cap_and_rejects_one_more() {
        // Arrange
        let wire = b"Content-Length: 0\r\n\r\n";
        let size = wire.len();
        for (max_header, ok) in [(size + 1, true), (size, true), (size - 1, false)] {
            let limits = Limits {
                max_header,
                max_body: 8,
            };

            // Act
            let got = read(wire, limits);

            // Assert
            match (ok, got) {
                (true, Ok(body)) => assert!(body.is_empty()),
                (false, Err(e @ FrameError::HeaderLimit { .. })) => {
                    assert!(e.to_string().contains("header limit"), "{e}");
                }
                (_, other) => panic!("limit {max_header}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_zero_length_body_extra_headers_and_lower_case_names_all_read() {
        for wire in [
            &b"Content-Length: 0\r\n\r\n"[..],
            b"Content-Type: x\r\nContent-Length: 0\r\n\r\n",
            b"content-length: 0\r\n\r\n",
        ] {
            // Act
            let body = read(wire, Limits::default()).unwrap();

            // Assert
            assert!(body.is_empty(), "{wire:?}");
        }
    }

    #[test]
    fn a_reader_that_yields_one_byte_per_call_still_gives_the_frame() {
        // Arrange
        let wire = b"Content-Length: 5\r\n\r\nhello".to_vec();
        let mut reader = BufReader::with_capacity(1, OneByte(Cursor::new(wire)));

        // Act
        let body = read_message(&mut reader, Limits::default()).unwrap();

        // Assert
        assert_eq!(body, b"hello");
    }

    #[test]
    fn malformed_frames_each_return_their_own_error() {
        let cases: [(&[u8], &str); 5] = [
            (b"X: 1\r\n\r\nabc", "Content-Length"),
            (b"Content-Length: abc\r\n\r\n", "Content-Length"),
            (b"Content-Length: 10\r\n\r\nabc", "end of input"),
            (b"Content-Length: 3\r\n", "end of input"),
            (b"", "closed"),
        ];
        for (wire, text) in cases {
            // Act
            let err = read(wire, Limits::default()).unwrap_err();

            // Assert
            assert!(err.to_string().contains(text), "{wire:?}: {err}");
        }
    }
}
