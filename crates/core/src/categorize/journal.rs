use super::*;
use fs2::FileExt;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Seek, SeekFrom, Write};

const MAX_JOURNAL: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Planned,
    Moved,
    Undoing,
    Restored,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Move {
    id: u64,
    name: PathBuf,
    category: Category,
    snapshot: Snapshot,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event")]
enum Event {
    Folder { version: u32, folder: PathBuf },
    Plan { movement: Move },
    Status { id: u64, state: State },
}

struct Journal {
    file: File,
    moves: BTreeMap<u64, (Move, State)>,
    next: u64,
    writable: bool,
}

impl Journal {
    fn open(folder: &Path, data: &Path) -> Result<Self> {
        let directory = data.join("organization");
        // Check existing ancestors before creating anything, then recheck the
        // completed directory. Never write a journal through a link/junction.
        if let Some(existing) = directory.ancestors().find(|p| p.exists()) {
            safe_directory(existing)?;
        }
        fs::create_dir_all(&directory)?;
        let directory = safe_directory(&directory)?;
        let digest = Sha256::digest(folder.as_os_str().as_encoded_bytes());
        let path = directory.join(format!("{digest:x}.jsonl"));
        let file = platform::open_journal(&path)?;
        ensure!(
            file.metadata()?.is_file() && !platform::is_link(&file.metadata()?),
            "journal is not a regular file"
        );
        FileExt::try_lock_exclusive(&file)
            .context("another organization or undo is running for this folder")?;
        platform::sync_directory(&directory)?;
        let mut journal = Self {
            file,
            moves: BTreeMap::new(),
            next: 1,
            writable: true,
        };
        ensure!(
            journal.file.metadata()?.len() <= MAX_JOURNAL,
            "organization journal is too large"
        );
        let mut bytes = Vec::new();
        journal.file.read_to_end(&mut bytes)?;
        let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        // A power loss can leave a partial final record. No mutation starts
        // until its complete write-ahead record has been synced, so discard it.
        if complete != bytes.len() {
            journal.file.set_len(complete as u64)?;
            journal.file.sync_all()?;
        }
        let mut lines = bytes[..complete]
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty());
        if let Some(first) = lines.next() {
            match serde_json::from_slice::<Event>(first).context("invalid journal header")? {
                Event::Folder {
                    version: 1,
                    folder: recorded,
                } if recorded == folder => {}
                _ => bail!("journal belongs to another folder or unsupported version"),
            }
            for line in lines {
                match serde_json::from_slice::<Event>(line).context("invalid journal record")? {
                    Event::Plan { movement } => {
                        validate_name(&movement.name)?;
                        ensure!(
                            !journal.moves.contains_key(&movement.id),
                            "duplicate journal movement"
                        );
                        ensure!(movement.id < u64::MAX, "invalid journal movement id");
                        journal.next = journal.next.max(movement.id + 1);
                        journal
                            .moves
                            .insert(movement.id, (movement, State::Planned));
                    }
                    Event::Status { id, state } => {
                        let movement = journal
                            .moves
                            .get_mut(&id)
                            .context("journal references an unknown movement")?;
                        movement.1 = state;
                    }
                    Event::Folder { .. } => bail!("unexpected journal header"),
                }
            }
        } else {
            journal.append(&Event::Folder {
                version: 1,
                folder: folder.to_path_buf(),
            })?;
        }
        Ok(journal)
    }

    fn append(&mut self, event: &Event) -> Result<()> {
        let mut bytes = serde_json::to_vec(event)?;
        bytes.push(b'\n');
        ensure!(
            self.file.metadata()?.len() + bytes.len() as u64 <= MAX_JOURNAL,
            "organization journal is full"
        );
        ensure!(
            self.writable,
            "journal write previously failed; retry after recovery"
        );
        self.writable = false;
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        self.writable = true;
        Ok(())
    }

    fn status(&mut self, id: u64, state: State) -> Result<()> {
        self.append(&Event::Status { id, state })?;
        self.moves
            .get_mut(&id)
            .context("unknown journal movement")?
            .1 = state;
        Ok(())
    }

    fn plan(&mut self, name: PathBuf, category: Category, snapshot: Snapshot) -> Result<Move> {
        let movement = Move {
            id: self.next,
            name,
            category,
            snapshot,
        };
        // Leave room for move completion, interrupted-operation recovery, and
        // undo for every outstanding file before permitting a new mutation.
        // A full history stops new organization, never restoration.
        let outstanding = self
            .moves
            .values()
            .filter(|(_, state)| !matches!(state, State::Restored | State::Abandoned))
            .count() as u64
            + 1;
        let plan_bytes = serde_json::to_vec(&Event::Plan {
            movement: movement.clone(),
        })?
        .len() as u64
            + 1;
        ensure!(
            self.file.metadata()?.len() + plan_bytes + outstanding * 1024 <= MAX_JOURNAL,
            "organization journal has no room for new moves; existing moves can still be undone"
        );
        self.append(&Event::Plan {
            movement: movement.clone(),
        })?;
        self.moves
            .insert(movement.id, (movement.clone(), State::Planned));
        self.next += 1;
        Ok(movement)
    }

    fn recover(&mut self, folder: &Path) -> Result<Vec<String>> {
        let pending: Vec<_> = self
            .moves
            .values()
            .filter(|(_, state)| matches!(state, State::Planned | State::Undoing))
            .cloned()
            .collect();
        let mut errors = Vec::new();
        for (movement, state) in pending {
            if let Err(error) = self.recover_one(folder, &movement, state) {
                errors.push(format!("{}: {error:#}", movement.name.display()));
            }
        }
        Ok(errors)
    }

    fn recover_one(&mut self, folder: &Path, movement: &Move, state: State) -> Result<()> {
        let (source, destination) = paths(folder, movement)?;
        let source_exists = exists(&source)?;
        let dest_exists = exists(&destination)?;
        match (source_exists, dest_exists) {
            (true, true) => {
                check_snapshot(&source, &movement.snapshot)?;
                check_snapshot(&destination, &movement.snapshot)?;
                ensure!(
                    same_file::is_same_file(&source, &destination)?,
                    "both locations contain different files; preserved both"
                );
                // An interrupted forward move is rolled back. An interrupted
                // undo is completed. Both leave the original location intact.
                fs::remove_file(&destination)?;
                platform::sync_directory(destination.parent().unwrap())?;
                self.status(
                    movement.id,
                    if state == State::Planned {
                        State::Abandoned
                    } else {
                        State::Restored
                    },
                )?;
            }
            (true, false) => {
                if state == State::Undoing {
                    check_snapshot(&source, &movement.snapshot)?;
                }
                self.status(
                    movement.id,
                    if state == State::Planned {
                        State::Abandoned
                    } else {
                        State::Restored
                    },
                )?;
            }
            (false, true) => {
                check_snapshot(&destination, &movement.snapshot)?;
                // Removal may have committed before its final status record.
                self.status(movement.id, State::Moved)?;
            }
            (false, false) => bail!("neither original nor organized file exists; journal retained"),
        }
        Ok(())
    }
}

fn validate_name(name: &Path) -> Result<()> {
    ensure!(
        name.parent() == Some(Path::new("")) && basename(name)? == name.as_os_str(),
        "invalid journal filename"
    );
    Ok(())
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn paths(folder: &Path, movement: &Move) -> Result<(PathBuf, PathBuf)> {
    validate_name(&movement.name)?;
    ensure!(safe_directory(folder)? == folder, "source folder changed");
    let target = folder.join(movement.category.folder_name());
    if exists(&target)? {
        safe_directory(&target)?;
    }
    Ok((folder.join(&movement.name), target.join(&movement.name)))
}

fn target_directory(folder: &Path, category: Category) -> Result<PathBuf> {
    ensure!(safe_directory(folder)? == folder, "source folder changed");
    let target = folder.join(category.folder_name());
    match fs::create_dir(&target) {
        Ok(()) => {
            platform::sync_directory(folder)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    safe_directory(&target)?;
    Ok(target)
}

fn move_one(
    journal: &mut Journal,
    folder: &Path,
    entry: &Entry,
    category: Category,
    expected: &Snapshot,
) -> Result<()> {
    check_snapshot(&entry.source, expected)?;
    let target = target_directory(folder, category)?;
    let name = PathBuf::from(basename(&entry.source)?);
    let destination = target.join(&name);
    ensure!(
        !exists(&destination)?,
        "destination already exists; left original untouched"
    );
    let movement = journal.plan(name, category, expected.clone())?;
    // Hard-link creation is atomic and fails on an existing destination. It
    // cannot overwrite anything, unlike rename, and requires the same volume.
    if let Err(error) = fs::hard_link(&entry.source, &destination) {
        journal.status(movement.id, State::Abandoned)?;
        return Err(error).context("could not create destination without overwriting");
    }
    // If any check or sync fails, retain both links and the pending journal.
    // Recovery will safely roll back the destination on the next operation.
    check_snapshot(&entry.source, expected)?;
    check_snapshot(&destination, expected)?;
    ensure!(
        same_file::is_same_file(&entry.source, &destination)?,
        "source changed during move; preserved both locations"
    );
    platform::sync_file(&destination)?;
    platform::sync_directory(&target)?;
    ensure!(safe_directory(folder)? == folder, "source folder changed");
    safe_directory(&target)?;
    check_snapshot(&entry.source, expected)?;
    ensure!(
        same_file::is_same_file(&entry.source, &destination)?,
        "source changed during move"
    );
    fs::remove_file(&entry.source)?;
    platform::sync_directory(folder)?;
    journal.status(movement.id, State::Moved)?;
    Ok(())
}

pub(super) fn organize(preview: &Preview, data: &Path) -> Result<String> {
    ensure!(
        preview.folder == preview.original_folder,
        "preview folder was changed"
    );
    let folder = safe_directory(&preview.folder)?;
    ensure!(
        folder == preview.original_folder,
        "preview folder changed since classification"
    );
    let mut journal = Journal::open(&folder, data)?;
    let mut errors = journal.recover(&folder)?;
    // Unresolved recovery must not be followed by additional mutations.
    ensure!(
        errors.is_empty(),
        "interrupted operation needs attention: {}",
        errors.join("; ")
    );
    let mut moved = 0;
    let mut skipped = 0;
    let mut seen = BTreeSet::new();
    for entry in &preview.entries {
        let Some(category) = entry.category else {
            skipped += 1;
            continue;
        };
        if entry.error.is_some() {
            skipped += 1;
            continue;
        }
        let result = (|| {
            ensure!(
                entry.source.parent() == Some(folder.as_path()),
                "entry is outside preview folder"
            );
            ensure!(seen.insert(entry.source.clone()), "duplicate preview entry");
            let expected = preview
                .snapshots
                .get(&entry.source)
                .context("entry was not present in original preview")?;
            move_one(&mut journal, &folder, entry, category, expected)
        })();
        match result {
            Ok(()) => moved += 1,
            Err(error) => errors.push(format!(
                "{}: {error:#}",
                entry
                    .source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            )),
        }
        ensure!(
            journal.writable,
            "journal write failed; stop and retry undo to recover safely"
        );
    }
    Ok(summary("Moved", moved, skipped, &errors))
}

fn restore_one(journal: &mut Journal, folder: &Path, movement: &Move) -> Result<()> {
    let (source, destination) = paths(folder, movement)?;
    check_snapshot(&destination, &movement.snapshot)?;
    ensure!(
        !exists(&source)?,
        "original filename is occupied; organized file preserved"
    );
    journal.status(movement.id, State::Undoing)?;
    fs::hard_link(&destination, &source).context("could not restore without overwriting")?;
    check_snapshot(&destination, &movement.snapshot)?;
    check_snapshot(&source, &movement.snapshot)?;
    ensure!(
        same_file::is_same_file(&source, &destination)?,
        "organized file changed during undo"
    );
    platform::sync_file(&source)?;
    platform::sync_directory(folder)?;
    safe_directory(destination.parent().unwrap())?;
    check_snapshot(&destination, &movement.snapshot)?;
    ensure!(
        same_file::is_same_file(&source, &destination)?,
        "organized file changed during undo"
    );
    fs::remove_file(&destination)?;
    platform::sync_directory(destination.parent().unwrap())?;
    journal.status(movement.id, State::Restored)?;
    Ok(())
}

pub(super) fn undo(folder: &Path, data: &Path) -> Result<String> {
    let folder = safe_directory(folder)?;
    let mut journal = Journal::open(&folder, data)?;
    let mut errors = journal.recover(&folder)?;
    let movements: Vec<_> = journal
        .moves
        .values()
        .rev()
        .filter(|(_, state)| *state == State::Moved)
        .map(|(m, _)| m.clone())
        .collect();
    let mut restored = 0;
    for movement in movements {
        match restore_one(&mut journal, &folder, &movement) {
            Ok(()) => restored += 1,
            Err(error) => errors.push(format!("{}: {error:#}", movement.name.display())),
        }
        ensure!(
            journal.writable,
            "journal write failed; retry undo to recover safely"
        );
    }
    Ok(summary("Restored", restored, 0, &errors))
}

fn summary(verb: &str, done: usize, skipped: usize, errors: &[String]) -> String {
    let mut summary = format!(
        "{verb} {done} file(s); skipped {skipped}; {} error(s).",
        errors.len()
    );
    for error in errors {
        summary.push('\n');
        summary.push_str(error);
    }
    summary
}

#[cfg(test)]
pub(super) fn simulate_interruption(
    preview: &Preview,
    data: &Path,
    removed: bool,
    undoing: bool,
) -> Result<()> {
    let mut journal = Journal::open(&preview.folder, data)?;
    let entry = &preview.entries[0];
    let category = entry.category.unwrap();
    let target = target_directory(&preview.folder, category)?;
    let movement = journal.plan(
        PathBuf::from(basename(&entry.source)?),
        category,
        preview.snapshots[&entry.source].clone(),
    )?;
    let destination = target.join(&movement.name);
    fs::hard_link(&entry.source, &destination)?;
    if removed {
        fs::remove_file(&entry.source)?;
    }
    if undoing {
        journal.status(movement.id, State::Moved)?;
        journal.status(movement.id, State::Undoing)?;
        if removed {
            fs::hard_link(&destination, &entry.source)?;
        }
    }
    Ok(())
}
