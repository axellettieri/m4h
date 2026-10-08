//! Text console.

use crate::Error;

/// The console: serial port on bare metal, stdin/stdout when hosted.
pub trait Console {
    /// Writes a string.
    fn write_str(&self, s: &str) -> Result<(), Error>;

    /// Reads available input into `buf`, blocking until at least one byte is
    /// available. Returns `0` at end of input.
    fn read(&self, buf: &mut [u8]) -> Result<usize, Error>;
}
