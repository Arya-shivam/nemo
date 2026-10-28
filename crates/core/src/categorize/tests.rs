use super::*;
use std::io::Write;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf, PathBuf, Preview) {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("selected");
    let data = temp.path().join("app-data");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("invoice.txt"), b"original invoice data").unwrap();
    let mut preview = inspect(&folder).unwrap();
    preview.entries[0].category = Some(Category::Finance);
    (temp, folder, data, preview)
}

#[test]
fn near_full_history_refuses_new_moves_but_allows_undo() {
    let (_temp, folder, data, preview) = fixture();
    journal::organize(&preview, &data).unwrap();
    let log = fs::read_dir(data.join("organization"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut file = fs::OpenOptions::new().append(true).open(log).unwrap();
    let remaining = 32 * 1024 * 1024 - 1024 - file.metadata().unwrap().len();
    let padding = vec![b'\n'; remaining as usize];
    file.write_all(&padding).unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs::write(folder.join("second.txt"), b"second file").unwrap();
    let mut next = inspect(&folder).unwrap();
    next.entries[0].category = Some(Category::Finance);
    let result = journal::organize(&next, &data).unwrap();
    assert!(result.contains("no room for new moves"));
    assert_eq!(fs::read(folder.join("second.txt")).unwrap(), b"second file");
    assert!(
        journal::undo(&folder, &data)
            .unwrap()
            .starts_with("Restored 1")
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
}

#[test]
fn real_move_and_persistent_undo() {
    let (_temp, folder, data, preview) = fixture();
    assert!(
        journal::organize(&preview, &data)
            .unwrap()
            .starts_with("Moved 1")
    );
    assert!(!folder.join("invoice.txt").exists());
    assert_eq!(
        fs::read(
            folder
                .join(Category::Finance.folder_name())
                .join("invoice.txt")
        )
        .unwrap(),
        b"original invoice data"
    );
    drop(preview);
    assert!(
        journal::undo(&folder, &data)
            .unwrap()
            .starts_with("Restored 1")
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
    assert!(
        journal::undo(&folder, &data)
            .unwrap()
            .starts_with("Restored 0")
    );
}

#[test]
fn destination_collision_never_overwrites() {
    let (_temp, folder, data, preview) = fixture();
    let target = folder.join(Category::Finance.folder_name());
    fs::create_dir(&target).unwrap();
    fs::write(target.join("invoice.txt"), b"user's existing file").unwrap();
    let summary = journal::organize(&preview, &data).unwrap();
    assert!(summary.starts_with("Moved 0"));
    assert!(summary.contains("destination already exists"));
    assert_eq!(
        fs::read(target.join("invoice.txt")).unwrap(),
        b"user's existing file"
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
}

#[test]
fn changed_source_is_not_moved() {
    let (_temp, folder, data, preview) = fixture();
    fs::write(folder.join("invoice.txt"), b"new data after preview").unwrap();
    assert!(
        journal::organize(&preview, &data)
            .unwrap()
            .contains("file changed since preview")
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"new data after preview"
    );
    assert!(
        !folder
            .join(Category::Finance.folder_name())
            .join("invoice.txt")
            .exists()
    );
}

#[test]
fn changed_organized_file_is_preserved_by_undo() {
    let (_temp, folder, data, preview) = fixture();
    journal::organize(&preview, &data).unwrap();
    let target = folder
        .join(Category::Finance.folder_name())
        .join("invoice.txt");
    fs::write(&target, b"edited while organized").unwrap();
    assert!(
        journal::undo(&folder, &data)
            .unwrap()
            .contains("file changed since preview")
    );
    assert_eq!(fs::read(target).unwrap(), b"edited while organized");
    assert!(!folder.join("invoice.txt").exists());
}

#[test]
fn occupied_original_is_preserved_by_undo() {
    let (_temp, folder, data, preview) = fixture();
    journal::organize(&preview, &data).unwrap();
    fs::write(folder.join("invoice.txt"), b"new user file").unwrap();
    assert!(
        journal::undo(&folder, &data)
            .unwrap()
            .contains("original filename is occupied")
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"new user file"
    );
    assert_eq!(
        fs::read(
            folder
                .join(Category::Finance.folder_name())
                .join("invoice.txt")
        )
        .unwrap(),
        b"original invoice data"
    );
}

#[test]
fn interrupted_link_is_rolled_back() {
    let (_temp, folder, data, preview) = fixture();
    journal::simulate_interruption(&preview, &data, false, false).unwrap();
    journal::undo(&folder, &data).unwrap();
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
    assert!(
        !folder
            .join(Category::Finance.folder_name())
            .join("invoice.txt")
            .exists()
    );
}

#[test]
fn interrupted_removal_is_recovered_and_undone() {
    let (_temp, folder, data, preview) = fixture();
    journal::simulate_interruption(&preview, &data, true, false).unwrap();
    assert!(
        journal::undo(&folder, &data)
            .unwrap()
            .starts_with("Restored 1")
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
}

#[test]
fn interrupted_undo_completes_safely() {
    let (_temp, folder, data, preview) = fixture();
    journal::simulate_interruption(&preview, &data, true, true).unwrap();
    journal::undo(&folder, &data).unwrap();
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
    assert!(
        !folder
            .join(Category::Finance.folder_name())
            .join("invoice.txt")
            .exists()
    );
}

#[test]
fn partial_final_journal_record_is_ignored() {
    let (_temp, folder, data, preview) = fixture();
    journal::organize(&preview, &data).unwrap();
    let path = fs::read_dir(data.join("organization"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"{\"event\":\"Status\",\"id\":")
        .unwrap();
    assert!(
        journal::undo(&folder, &data)
            .unwrap()
            .starts_with("Restored 1")
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
}

#[test]
fn excludes_sensitive_incomplete_hidden_and_nested_files() {
    let temp = tempfile::tempdir().unwrap();
    for name in [
        ".env",
        ".env.production",
        "credentials.json",
        "my-passwords.txt",
        "id_rsa",
        "cert.pem",
        "file.crdownload",
        "file.part",
        "invoice.txt",
    ] {
        fs::write(temp.path().join(name), b"data").unwrap();
    }
    fs::create_dir(temp.path().join("nested")).unwrap();
    fs::write(temp.path().join("nested/visible.txt"), b"nested").unwrap();
    let preview = inspect(temp.path()).unwrap();
    assert_eq!(preview.entries.len(), 1);
    assert_eq!(
        preview.entries[0].source.file_name().unwrap(),
        "invoice.txt"
    );
}

#[test]
fn category_none_excludes_file_and_modified_preview_cannot_add_sources() {
    let (_temp, folder, data, mut preview) = fixture();
    preview.entries[0].category = None;
    assert!(
        journal::organize(&preview, &data)
            .unwrap()
            .starts_with("Moved 0")
    );
    fs::write(folder.join("new.txt"), b"new file").unwrap();
    preview.entries.push(Entry {
        source: preview.folder.join("new.txt"),
        category: Some(Category::Work),
        error: None,
    });
    assert!(
        journal::organize(&preview, &data)
            .unwrap()
            .contains("not present in original preview")
    );
    assert!(folder.join("new.txt").exists());
}

fn answer() -> Value {
    let probabilities: BTreeMap<_, _> = Category::ALL.into_iter().map(|c| (c.key(), 0.1)).collect();
    json!({"model": MODEL, "answers": {
        "category": {"type": "choice", "choice": "finance", "confidence": 0.8, "probabilities": probabilities},
        "valid": {"type": "noul", "noul": 0.9}
    }})
}

#[test]
fn validates_external_category_and_validity() {
    assert_eq!(
        parse_answer(&serde_json::to_vec(&answer()).unwrap()).unwrap(),
        Category::Finance
    );
    for choice in ["../escape", "unknown", "Finance", ""] {
        let mut value = answer();
        value["answers"]["category"]["choice"] = json!(choice);
        assert!(parse_answer(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for validity in [-0.1, 0.69, 1.1] {
        let mut value = answer();
        value["answers"]["valid"]["noul"] = json!(validity);
        assert!(parse_answer(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value = answer();
    value["answers"]["category"]["probabilities"]["finance"] = json!(2.0);
    assert!(parse_answer(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn image_evidence_is_reencoded_and_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("large.png");
    image::RgbImage::new(1600, 1100).save(&path).unwrap();
    let bytes = image_evidence(&path).unwrap().unwrap();
    assert!(bytes.starts_with(&[0xff, 0xd8]));
    let image = image::load_from_memory(&bytes).unwrap();
    assert!(image.width() <= 1024 && image.height() <= 1024);
    let unknown = temp.path().join("document.txt");
    fs::write(&unknown, b"never send document contents").unwrap();
    assert!(image_evidence(&unknown).unwrap().is_none());
}

#[test]
fn cancellation_makes_no_request_and_no_mutation() {
    let (_temp, folder, _data, _preview) = fixture();
    let cancelled = AtomicBool::new(true);
    assert!(
        preview(
            &folder,
            Evidence::NamesOnly,
            "not-a-real-key",
            &cancelled,
            |_, _| {}
        )
        .is_err()
    );
    assert_eq!(
        fs::read(folder.join("invoice.txt")).unwrap(),
        b"original invoice data"
    );
}
