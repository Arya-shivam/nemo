use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use gyotaku_core::{Config, status};

use crate::clipboard::Clipboard;
use crate::indexer::{self, Indexer, Outcome};
use crate::platform;

// How long a file has to sit untouched before it gets read. Some tools write a
// screenshot in several passes, and reading a half written png just fails.
const SETTLE: Duration = Duration::from_millis(400);

struct Watch {
    indexer: Indexer,
    /// The folders being read right now.
    folders: Vec<PathBuf>,
    /// Files that changed recently, read once they've been quiet for SETTLE.
    pending: HashMap<PathBuf, Instant>,
    /// Files (or folders) that were renamed away. inotify reports the old
    /// name first and only pairs it with the new one in a later event, so
    /// forgetting right away would throw out the text of a file that only
    /// moved.
    leaving: HashMap<PathBuf, Instant>,
    /// The last thing renamed away, for pairing with the new name when the
    /// two arrive as separate events.
    last_from: Option<PathBuf>,
    /// Folders that appeared (made, or moved in) whose files no event will
    /// ever mention: the watch on them starts after they were filled.
    arrived: Vec<PathBuf>,
    /// Set when events may have been lost (the kernel's queue overflowed, or
    /// the watcher hit an error), so everything gets checked again.
    rescan: bool,
}

/// Folders given on the command line are fixed. Without them the watcher
/// follows the config file, so adding or removing a folder in the app takes
/// effect here straight away, no restart.
pub fn run(fixed: Option<Vec<PathBuf>>, threads: Option<usize>) -> Result<()> {
    let Some(_lock) = only_watcher()? else {
        eprintln!("another gyotaku watch is already running, leaving it to that one");
        return Ok(());
    };
    platform::become_idle();
    let config_path = Config::path()?;
    let config = Config::load_or_default();
    let mut threads_now = threads.unwrap_or(config.threads);
    let mut scripts_now = config.scripts();
    let follow_config = fixed.is_none();

    status::set("getting the text reader ready, the first time this downloads 22 MB");
    let indexer = match Indexer::new(threads_now, &scripts_now) {
        Ok(indexer) => indexer,
        Err(e) => {
            status::set(&format!("couldn't get the text reader ready: {e:#}"));
            return Err(e);
        }
    };
    // Folders given by hand mean the config isn't followed, the clipboard
    // setting included. Followed, this makes the clipboard folder before it's
    // watched.
    let mut clipboard = Clipboard::new();
    if follow_config {
        clipboard.follow(&config);
    }
    let fresh = clipboard.fresh();
    let mut w = Watch {
        indexer,
        folders: fixed.clone().unwrap_or(config.reading_folders()),
        pending: HashMap::new(),
        leaving: HashMap::new(),
        last_from: None,
        arrived: Vec::new(),
        rescan: false,
    };
    w.forget_the_gone(follow_config);

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if let Ok(event) = &event {
            fresh.note(event);
        }
        let _ = tx.send(event);
    })?;
    for dir in &w.folders {
        watch_folder(&mut watcher, dir);
    }
    if follow_config && let Some(dir) = config_path.parent() {
        std::fs::create_dir_all(dir)?;
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }

    // Only what actually needs reading, so a restart over a library that's
    // already read has nothing to do and nothing to report.
    let mut backlog: VecDeque<PathBuf> = indexer::scan(&w.folders)
        .into_iter()
        .filter(|p| w.indexer.needs_reading(p))
        .collect();
    let mut caught_up = backlog.is_empty();
    // Shots read before bursts existed may still need theirs worked out.
    let mut settling = true;
    let mut power = Power::default();
    // How far through the backlog, for the window: (done, of).
    let mut progress = (0, backlog.len());
    status::set(if caught_up {
        "up to date"
    } else {
        "reading your screenshots"
    });

    loop {
        let mut config_changed = false;
        let mut take = |event: notify::Result<Event>, w: &mut Watch| match event {
            Ok(event) => {
                config_changed |= follow_config && event.paths.iter().any(|p| p == &config_path);
                w.on_event(event);
            }
            // Out of inotify watches for a new subfolder, a read error: some
            // events may be gone, but that's a reason to look again, not to
            // stop reading for good.
            Err(e) => {
                eprintln!("file watcher: {e}");
                w.rescan = true;
            }
        };
        // Block for as long as there is nothing else to do.
        let wait = if !w.pending.is_empty() || !w.leaving.is_empty() {
            SETTLE
        } else if !backlog.is_empty() || settling || !w.arrived.is_empty() || w.rescan {
            // Working through an old library can wait, a laptop's battery
            // matters more. Anything new still wakes this straight away.
            if power.on_battery() && (!backlog.is_empty() || settling) {
                BATTERY_PACE
            } else {
                Duration::ZERO
            }
        } else {
            Duration::from_secs(3600)
        };
        match rx.recv_timeout(wait) {
            Ok(event) => take(event, &mut w),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("file watcher stopped"),
        }
        while let Ok(event) = rx.try_recv() {
            take(event, &mut w);
        }

        // A config that doesn't parse (someone mid edit) is ignored until it does.
        if config_changed && let Ok(Some(config)) = Config::load_from(&config_path) {
            clipboard.follow(&config);
            let folders = config.reading_folders();
            let added: Vec<PathBuf> = folders
                .iter()
                .filter(|f| !w.folders.contains(f))
                .cloned()
                .collect();
            let gone: Vec<PathBuf> = w
                .folders
                .iter()
                .filter(|f| !folders.contains(f))
                .cloned()
                .collect();
            for gone in &gone {
                let _ = watcher.unwatch(gone);
                backlog
                    .retain(|p| !p.starts_with(gone) || folders.iter().any(|f| p.starts_with(f)));
                let dropped = w.forget_under(gone, &folders);
                eprintln!(
                    "stopped watching {} ({dropped} screenshots forgotten)",
                    gone.display()
                );
            }
            // Unwatching a folder also drops the watches on everything
            // inside it, including a folder that's still wanted
            // (~/Pictures/Screenshots after removing ~/Pictures).
            if !gone.is_empty() {
                for dir in folders.iter().filter(|f| !added.contains(f)) {
                    watch_folder(&mut watcher, dir);
                }
            }
            for dir in &added {
                watch_folder(&mut watcher, dir);
                // Newly added folders go to the front, it's what was just asked for.
                for path in indexer::scan(std::slice::from_ref(dir)).into_iter().rev() {
                    if w.indexer.needs_reading(&path) {
                        backlog.push_front(path);
                    }
                }
                caught_up = false;
            }
            let threads_wanted = threads.unwrap_or(config.threads);
            let scripts_wanted = config.scripts();
            if threads_wanted != threads_now || scripts_wanted != scripts_now {
                // A new script's model downloads here. Offline, reading goes
                // on as before, and the next config change tries again.
                if scripts_wanted != scripts_now {
                    status::set("getting the reader for another script ready");
                }
                match w.indexer.set_reader(threads_wanted, &scripts_wanted) {
                    Ok(()) => {
                        threads_now = threads_wanted;
                        scripts_now = scripts_wanted;
                        eprintln!(
                            "reading with {threads_now} threads and {} extra scripts now",
                            scripts_now.len()
                        );
                        if caught_up {
                            status::set("up to date");
                        }
                    }
                    Err(e) => {
                        eprintln!("couldn't change the reader: {e:#}");
                        status::set(&format!("couldn't get that script's reader: {e:#}"));
                    }
                }
            }
            w.folders = folders;
        }

        // A folder that appeared brings files no event mentioned, and lost
        // events mean anything could have changed: both are found by looking.
        let look: Vec<PathBuf> = if std::mem::take(&mut w.rescan) {
            w.arrived.clear();
            w.folders.clone()
        } else {
            std::mem::take(&mut w.arrived)
        };
        if !look.is_empty() {
            for path in indexer::scan(&look) {
                if w.reads(&path) && w.indexer.needs_reading(&path) && !backlog.contains(&path) {
                    backlog.push_back(path);
                    caught_up = false;
                }
            }
        }

        for path in settled(&mut w.leaving) {
            if !path.exists() {
                w.forget_all(&path);
            }
        }

        // Fresh screenshots jump the queue: the one you took just now is the
        // one you're about to look for, even while an old library backfills.
        let fresh: Vec<PathBuf> = settled(&mut w.pending)
            .into_iter()
            .filter(|p| p.is_file() && w.reads(p))
            .collect();
        if !fresh.is_empty() {
            for path in fresh {
                if index(&mut w.indexer, &path) == Read::Again {
                    w.pending.insert(path, Instant::now());
                }
            }
            continue;
        }

        if let Some(path) = backlog.pop_front() {
            index(&mut w.indexer, &path);
            progress.0 += 1;
            progress.1 = progress.1.max(progress.0 + backlog.len());
            status::set(&format!("reading {} of {}", progress.0, progress.1));
        }
        // Said as soon as the last one is done: the loop goes back to waiting
        // after this, possibly for an hour, and "reading 340 of 340" would
        // stand that whole time.
        if backlog.is_empty() && !caught_up {
            caught_up = true;
            progress = (0, 0);
            status::set("up to date");
            eprintln!(
                "caught up, {} screenshots searchable",
                w.indexer.index.visible_len()?
            );
        }

        // Last of all, with nothing left to read: bursts for shots read
        // before there were any, a batch at a time so a screenshot taken
        // meanwhile never waits on it for long. Quietly, the window shows
        // the shots either way, only not stacked yet.
        if backlog.is_empty() && settling {
            settling = match w.indexer.settle_some(SETTLE_BATCH) {
                Ok(n) => n > 0,
                Err(e) => {
                    eprintln!("working out similar screenshots: {e:#}");
                    false
                }
            };
        }
    }
}

// About 50 thumbnails decoded and compared, well under a second.
const SETTLE_BATCH: usize = 50;

/// One watcher at a time. A second would read every new screenshot again and
/// fight the first over the index, so it just leaves. The lock is held for
/// as long as this process lives and the OS drops it when it exits, however
/// it exits. The pid goes in a file of its own, since on Windows a locked
/// file can't even be read by anyone else.
fn only_watcher() -> Result<Option<std::fs::File>> {
    let dir = gyotaku_core::data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("watch.lock"))?;
    // The window takes the lock for an instant to check whether a reader is
    // running, so one failed try isn't proof another reader exists.
    for _ in 0..5 {
        match lock.try_lock() {
            Ok(()) => {
                std::fs::write(dir.join("watch.pid"), std::process::id().to_string())?;
                return Ok(Some(lock));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(200))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    Ok(None)
}

// On battery, one old screenshot every few seconds instead of flat out.
const BATTERY_PACE: Duration = Duration::from_secs(3);

/// Whether the machine is running on battery, looked up at most every 30
/// seconds. Desktops have none, so they're never throttled.
#[derive(Default)]
struct Power {
    checked: Option<Instant>,
    on_battery: bool,
}

impl Power {
    fn on_battery(&mut self) -> bool {
        if self
            .checked
            .is_none_or(|t| t.elapsed() > Duration::from_secs(30))
        {
            self.checked = Some(Instant::now());
            self.on_battery = platform::on_battery();
        }
        self.on_battery
    }
}

/// A folder that isn't there (an unplugged drive, a typo in the config) is
/// reported and skipped rather than taking the whole watcher down.
fn watch_folder(watcher: &mut impl Watcher, dir: &Path) {
    match watcher.watch(dir, RecursiveMode::Recursive) {
        Ok(()) => eprintln!("watching {}", dir.display()),
        Err(e) => eprintln!("can't watch {}: {e}", dir.display()),
    }
}

/// Takes out every entry that's been quiet for at least SETTLE.
fn settled(map: &mut HashMap<PathBuf, Instant>) -> Vec<PathBuf> {
    let now = Instant::now();
    let ready: Vec<PathBuf> = map
        .iter()
        .filter(|(_, t)| now.duration_since(**t) >= SETTLE)
        .map(|(p, _)| p.clone())
        .collect();
    for p in &ready {
        map.remove(p);
    }
    ready
}

impl Watch {
    /// Whether a path is one this reads: inside a configured folder, and not
    /// inside a hidden folder in there (the same rule the scan follows, which
    /// also keeps a drive's `.Trash-1000` out when the drive is the folder).
    fn reads(&self, path: &Path) -> bool {
        self.folders.iter().any(|folder| {
            path.strip_prefix(folder).is_ok_and(|rest| {
                !rest
                    .components()
                    .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
            })
        })
    }

    /// At startup, drops shots whose files are gone. Only where the folder
    /// they were in is still there: a drive that's unplugged right now keeps
    /// its index for when it's back, instead of being read all over again.
    /// Following the config, shots outside every folder are dropped too (a
    /// folder removed while nothing was running).
    fn forget_the_gone(&mut self, follow_config: bool) {
        let Ok(paths) = self.indexer.index.paths() else {
            return;
        };
        for path in paths {
            let folder = self.folders.iter().find(|f| path.starts_with(f));
            let gone = match folder {
                Some(folder) => folder.is_dir() && !path.exists(),
                None => follow_config,
            };
            if gone {
                forget(&mut self.indexer, &path);
            }
        }
    }

    /// Forgets every shot under `dir` that isn't also under one of `keep`
    /// (folders can nest). Returns how many.
    fn forget_under(&mut self, dir: &Path, keep: &[PathBuf]) -> usize {
        let outside = |p: &Path| p.starts_with(dir) && !keep.iter().any(|k| p.starts_with(k));
        self.pending.retain(|p, _| !outside(p));
        let paths = self.indexer.index.paths().unwrap_or_default();
        let gone: Vec<PathBuf> = paths.into_iter().filter(|p| outside(p)).collect();
        for p in &gone {
            let _ = self.indexer.forget(p);
        }
        gone.len()
    }

    /// A path that went away: the shot if it was one, everything inside it if
    /// it was a folder. Which it was can't be asked any more.
    fn forget_all(&mut self, path: &Path) {
        self.pending.retain(|p, _| !p.starts_with(path));
        if indexer::is_image(path) {
            forget(&mut self.indexer, path);
            return;
        }
        for shot in self.indexer.index.paths().unwrap_or_default() {
            if shot.starts_with(path) {
                forget(&mut self.indexer, &shot);
            }
        }
    }

    /// A folder renamed or moved within the watched folders: every shot in it
    /// keeps its text under the new path.
    fn move_folder(&mut self, from: &Path, to: &Path) {
        let mut moved = 0;
        for shot in self.indexer.index.paths().unwrap_or_default() {
            if let Ok(rest) = shot.strip_prefix(from)
                && self.indexer.rename(&shot, &to.join(rest)).unwrap_or(false)
            {
                moved += 1;
            }
        }
        if moved > 0 {
            eprintln!("moved {moved} screenshots to {}", to.display());
        }
        // Anything in there that wasn't read yet.
        self.arrived.push(to.to_path_buf());
    }

    /// `from` became `to`, both known.
    fn renamed(&mut self, from: &Path, to: &Path, now: Instant) {
        self.leaving.remove(from);
        if to.is_dir() {
            self.move_folder(from, to);
        } else if indexer::is_image(from) && indexer::is_image(to) && self.reads(to) {
            match self.indexer.rename(from, to) {
                Ok(true) => eprintln!("moved {} -> {}", from.display(), to.display()),
                Ok(false) => {
                    self.pending.insert(to.to_path_buf(), now);
                }
                Err(e) => eprintln!("failed to move {}: {e:#}", from.display()),
            }
        } else {
            if indexer::is_image(from) {
                forget(&mut self.indexer, from);
            }
            if indexer::is_image(to) && self.reads(to) {
                self.pending.insert(to.to_path_buf(), now);
            }
        }
    }

    fn on_event(&mut self, event: Event) {
        let now = Instant::now();
        if event.need_rescan() {
            self.rescan = true;
        }
        let watched = |p: &&PathBuf| indexer::is_image(p) && self.reads(p);
        let images: Vec<PathBuf> = event.paths.iter().filter(watched).cloned().collect();

        match event.kind {
            EventKind::Create(CreateKind::Folder) => {
                self.arrived.extend(event.paths.iter().cloned());
            }
            EventKind::Remove(RemoveKind::Folder) => {
                for path in &event.paths {
                    self.forget_all(path);
                }
            }
            // Windows only ever says "removed", folder or not, so a path with
            // no extension is taken to maybe be a folder.
            EventKind::Remove(_) => {
                for path in &event.paths {
                    if indexer::is_image(path) {
                        self.pending.remove(path);
                        forget(&mut self.indexer, path);
                    } else if path.extension().is_none() {
                        self.forget_all(path);
                    }
                }
            }
            // Something renamed away: maybe a shot, maybe a whole folder.
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                for path in &event.paths {
                    self.pending.remove(path);
                    self.leaving.insert(path.clone(), now);
                }
                self.last_from = event.paths.last().cloned();
            }
            // A rename inside the watched folders, finally with both names.
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
                let (from, to) = (event.paths[0].clone(), event.paths[1].clone());
                self.renamed(&from, &to, now);
            }
            // Windows never sends both names together: the new one comes
            // right after the old one, so they're paired here. inotify sends
            // the pair afterwards as well, which then finds nothing left to
            // do.
            EventKind::Modify(ModifyKind::Name(RenameMode::To))
                if self
                    .last_from
                    .as_ref()
                    .is_some_and(|from| self.leaving.contains_key(from)) =>
            {
                let from = self.last_from.take().expect("checked above");
                for to in &event.paths {
                    self.renamed(&from, to, now);
                }
            }
            // Moved in from outside: a folder brings files nobody mentions.
            EventKind::Modify(ModifyKind::Name(_)) => {
                for path in &event.paths {
                    if path.is_dir() {
                        self.arrived.push(path.clone());
                    } else if indexer::is_image(path) && self.reads(path) {
                        self.pending.insert(path.clone(), now);
                    }
                }
            }
            // The writer closed the file, so it's complete and there's nothing
            // to wait for. Marking it as already settled gets a fresh
            // screenshot read straight away instead of 400 ms later.
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                let done = now.checked_sub(SETTLE).unwrap_or(now);
                for path in images {
                    self.pending.insert(path, done);
                }
            }
            // Some platforms only say "created" for a folder too.
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in &event.paths {
                    if path.is_dir() && matches!(event.kind, EventKind::Create(_)) {
                        self.arrived.push(path.clone());
                    }
                }
                for path in images {
                    self.pending.insert(path, now);
                }
            }
            _ => {}
        }
    }
}

#[derive(PartialEq)]
enum Read {
    Done,
    /// Couldn't be read yet (still being written), worth another try.
    Again,
}

fn index(indexer: &mut Indexer, path: &Path) -> Read {
    match indexer.index_file(path) {
        Ok(Outcome::Indexed { lines, took }) => {
            eprintln!("indexed {} ({lines} lines, {took:.1?})", path.display());
        }
        Ok(Outcome::Visual) => eprintln!("visual embedding {}", path.display()),
        Ok(Outcome::Hidden(why)) => eprintln!("skipped {}: {why}", path.display()),
        Ok(Outcome::Unchanged | Outcome::Thumbnail) => {}
        Ok(Outcome::NotYet) => return Read::Again,
        // One bad file shouldn't take the watcher down with it.
        Err(e) => eprintln!("failed {}: {e:#}", path.display()),
    }
    Read::Done
}

fn forget(indexer: &mut Indexer, path: &Path) {
    match indexer.forget(path) {
        Ok(true) => eprintln!("forgot {}", path.display()),
        Ok(false) => {}
        Err(e) => eprintln!("failed to forget {}: {e:#}", path.display()),
    }
}
