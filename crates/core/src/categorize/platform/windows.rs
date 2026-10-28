use std::fs::{Metadata, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub fn is_link(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

pub(super) fn no_follow(options: &mut OpenOptions) {
    options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
}

pub fn sync_file(path: &Path) -> io::Result<()> {
    // FlushFileBuffers requires a write-capable handle on Windows.
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    no_follow(&mut options);
    options.open(path)?.sync_all()
}

pub fn sync_directory(path: &Path) -> io::Result<()> {
    // Windows cannot portably fsync directories. File contents are flushed
    // separately; filesystem journaling provides namespace durability.
    let _directory = OpenOptions::new()
        .read(true)
        .custom_flags(0x02000000)
        .open(path)?;
    Ok(())
}
