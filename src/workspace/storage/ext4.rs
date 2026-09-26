use std::io::{Read, Seek, SeekFrom};

use super::*;

/// ext2/3/4 superblock magic.
const EXT4_SUPERBLOCK_MAGIC: u16 = 0xEF53;
/// The superblock starts 1024 bytes into the filesystem.
const EXT4_SUPERBLOCK_OFFSET: u64 = 1024;
const EXT4_SUPERBLOCK_SIZE: usize = 1024;
/// `s_feature_incompat` bit that enables the high half of the block count.
const EXT4_FEATURE_INCOMPAT_64BIT: u32 = 0x80;

/// What a filesystem's superblock says it is.
///
/// The block count is what a resize has to be expressed in: `resize2fs` takes a
/// size in blocks, so rounding a requested byte count down to a whole number of
/// blocks is what keeps the filesystem and the image file the same size rather
/// than leaving a few stray bytes at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ext4Geometry {
    pub(crate) block_size: u64,
    pub(crate) block_count: u64,
}

impl Ext4Geometry {
    pub(crate) fn size_bytes(&self) -> u64 {
        self.block_count.saturating_mul(self.block_size)
    }

    /// The largest whole number of blocks that fits in `bytes`.
    pub(crate) fn blocks_in(&self, bytes: u64) -> u64 {
        bytes / self.block_size
    }
}

/// Size of the filesystem recorded in the image's ext2/3/4 superblock.
///
/// `resize2fs` can fail after the image file has already been grown, which
/// leaves the image larger than the filesystem it contains. Reading the
/// superblock lets the resize path prove the filesystem reached the requested
/// size instead of trusting the image file size.
pub(crate) fn filesystem_size(image: &Path) -> Result<u64> {
    Ok(geometry(image)?.size_bytes())
}

/// The block size and block count the image's superblock declares.
pub(crate) fn geometry(image: &Path) -> Result<Ext4Geometry> {
    let mut file = fs::File::open(image)
        .with_context(|| format!("failed to open workspace disk image {}", image.display()))?;
    let mut superblock = [0u8; EXT4_SUPERBLOCK_SIZE];
    file.seek(SeekFrom::Start(EXT4_SUPERBLOCK_OFFSET))
        .with_context(|| format!("failed to seek in workspace disk image {}", image.display()))?;
    file.read_exact(&mut superblock).with_context(|| {
        format!(
            "workspace disk image {} is too small to hold an ext4 superblock",
            image.display()
        )
    })?;
    parse_geometry(&superblock)
        .with_context(|| format!("failed to read the filesystem in {}", image.display()))
}

#[cfg(test)]
pub(crate) fn parse_superblock(superblock: &[u8; EXT4_SUPERBLOCK_SIZE]) -> Result<u64> {
    Ok(parse_geometry(superblock)?.size_bytes())
}

pub(crate) fn parse_geometry(superblock: &[u8; EXT4_SUPERBLOCK_SIZE]) -> Result<Ext4Geometry> {
    let magic = read_u16(superblock, 0x38);
    if magic != EXT4_SUPERBLOCK_MAGIC {
        bail!("not an ext2/3/4 filesystem (superblock magic 0x{magic:04x})");
    }
    let blocks_low = u64::from(read_u32(superblock, 0x04));
    let log_block_size = read_u32(superblock, 0x18);
    if log_block_size > 6 {
        bail!("unsupported ext4 block size shift {log_block_size}");
    }
    let block_size = 1024u64 << log_block_size;
    let block_count = if read_u32(superblock, 0x60) & EXT4_FEATURE_INCOMPAT_64BIT != 0 {
        blocks_low | (u64::from(read_u32(superblock, 0x150)) << 32)
    } else {
        blocks_low
    };
    Ok(Ext4Geometry {
        block_size,
        block_count,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/storage/ext4.rs"]
mod tests;
