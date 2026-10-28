//! Platform-specific no-follow opens and directory durability.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::no_follow;
#[cfg(target_os = "linux")]
pub use linux::{is_link, sync_directory, sync_file};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::no_follow;
#[cfg(windows)]
pub use windows::{is_link, sync_directory, sync_file};
#[cfg(not(any(target_os = "linux", windows)))]
compile_error!("categorization requires a platform implementation for this operating system");

pub fn open_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    options.open(path)
}

pub fn open_journal(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    no_follow(&mut options);
    options.open(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn rejects_linked_folders_sources_and_category_directories() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("selected");
        let outside = temp.path().join("outside");
        fs::create_dir(&folder).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("private.txt"), b"private contents").unwrap();
        symlink(outside.join("private.txt"), folder.join("linked.txt")).unwrap();
        assert!(inspect(&folder).unwrap().entries.is_empty());
        let linked_folder = temp.path().join("linked-folder");
        symlink(&folder, &linked_folder).unwrap();
        assert!(inspect(&linked_folder).is_err());
        fs::write(folder.join("invoice.txt"), b"real data").unwrap();
        let mut preview = inspect(&folder).unwrap();
        preview.entries[0].category = Some(Category::Finance);
        symlink(&outside, folder.join(Category::Finance.folder_name())).unwrap();
        let result = journal::organize(&preview, &temp.path().join("app-data")).unwrap();
        assert!(result.starts_with("Moved 0"));
        assert!(folder.join("invoice.txt").exists());
        assert!(!outside.join("invoice.txt").exists());
        assert_eq!(
            fs::read(outside.join("private.txt")).unwrap(),
            b"private contents"
        );
    }
}
