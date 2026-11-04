// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A reader and writer pair that carries framed messages. Production wires it
//! to a child's stdio; tests wire it to in-process pipes.

use super::framing::{read_message, write_message, FrameError, Limits};
use std::io::{self, BufRead, BufReader, Read, Write};

/// A boxed reader that can move to the thread that drains it.
pub type BoxedReader = Box<dyn BufRead + Send>;
/// A boxed writer that can move to the thread that feeds it.
pub type BoxedWriter = Box<dyn Write + Send>;

pub struct Transport {
    reader: BoxedReader,
    writer: BoxedWriter,
}

impl Transport {
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Self {
        Self {
            reader: Box::new(BufReader::new(reader)),
            writer: Box::new(writer),
        }
    }

    pub fn read(&mut self, limits: Limits) -> Result<Vec<u8>, FrameError> {
        read_message(&mut self.reader, limits)
    }

    pub fn write(&mut self, body: &[u8]) -> io::Result<()> {
        write_message(&mut self.writer, body)
    }

    /// Writes bytes that are not a frame. Only tests need malformed input.
    #[cfg(test)]
    pub(crate) fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Splits the pair so each half can live on its own thread.
    pub fn into_parts(self) -> (BoxedReader, BoxedWriter) {
        (self.reader, self.writer)
    }
}

/// Two connected transports: what one writes the other reads.
#[cfg(test)]
pub(crate) fn pipe_pair() -> io::Result<(Transport, Transport)> {
    let (to_server_read, to_server_write) = io::pipe()?;
    let (to_client_read, to_client_write) = io::pipe()?;
    Ok((
        Transport::new(to_client_read, to_server_write),
        Transport::new(to_server_read, to_client_write),
    ))
}
