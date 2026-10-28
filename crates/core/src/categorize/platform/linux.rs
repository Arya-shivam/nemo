use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub fn is_link(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

pub(super) fn no_follow(options: &mut OpenOptions) {
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}

pub fn sync_file(path: &Path) -> io::Result<()> {
    super::open_file(path)?.sync_all()
}

pub fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
