//! Minimal block storage, enough for Mnesia, DETS and logs.

use crate::Error;

/// A block device: a flat array of bytes with explicit flush.
pub trait BlockDevice {
    /// Preferred I/O granularity in bytes.
    fn block_size(&self) -> usize;

    /// Size of the device in bytes.
    fn len(&self) -> Result<u64, Error>;

    /// `true` if the device has no bytes.
    fn is_empty(&self) -> Result<bool, Error> {
        Ok(self.len()? == 0)
    }

    /// Reads into `buf` from `offset`. Returns the number of bytes read
    /// (short only at the end of the device).
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Error>;

    /// Writes `buf` at `offset`. Returns the number of bytes written.
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, Error>;

    /// Makes every completed write durable.
    fn flush(&self) -> Result<(), Error>;
}

/// Access to block devices by name.
pub trait Storage {
    /// The device type.
    type Device: BlockDevice;

    /// Opens the device `name`. With `create = Some(len)`, creates it with
    /// `len` zero bytes if it does not exist.
    fn open_device(&self, name: &str, create: Option<u64>) -> Result<Self::Device, Error>;
}
