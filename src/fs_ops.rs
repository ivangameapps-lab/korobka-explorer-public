use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct TrashManifestEntry {
    pub original: PathBuf,
    pub trashed: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub struct TrashManifest {
    pub entries: Vec<TrashManifestEntry>,
}

fn localized(ru: String, en: String) -> String {
    if crate::i18n::english() {
        en
    } else {
        ru
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    Trash,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Replace,
    KeepBoth,
    Skip,
    Cancel,
}

#[derive(Clone, Copy)]
pub struct ConflictDecision {
    pub choice: ConflictChoice,
    pub apply_to_all: bool,
}

impl OpKind {
    pub fn label(self) -> &'static str {
        if crate::i18n::english() {
            match self {
                Self::Copy => "Copying",
                Self::Move => "Moving",
                Self::Trash => "Moving to trash",
                Self::Delete => "Deleting",
            }
        } else {
            match self {
                Self::Copy => "Копирование",
                Self::Move => "Перемещение",
                Self::Trash => "Удаление в корзину",
                Self::Delete => "Удаление",
            }
        }
    }
}

pub enum OpMessage {
    Progress {
        done: usize,
        total: usize,
        current: PathBuf,
    },
    Conflict {
        source: PathBuf,
        target: PathBuf,
    },
    Finished {
        kind: OpKind,
        completed: Vec<(PathBuf, PathBuf)>,
        skipped: Vec<(PathBuf, String)>,
        replaced: bool,
        elapsed: Duration,
    },
    Failed(String),
}

pub struct OpState {
    pub receiver: Receiver<OpMessage>,
    pub cancel: Arc<AtomicBool>,
    pub decisions: SyncSender<ConflictDecision>,
}

pub fn start(kind: OpKind, sources: Vec<PathBuf>, destination: PathBuf) -> Result<OpState, String> {
    if sources.is_empty() {
        return Err(crate::i18n::tr("Ничего не выбрано").to_owned());
    }
    let (sender, receiver) = mpsc::sync_channel(64);
    let (decisions, decision_receiver) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);

    thread::Builder::new()
        .name("korobka-ops".to_owned())
        .spawn(move || {
            run(
                kind,
                sources,
                destination,
                worker_cancel,
                sender,
                decision_receiver,
            )
        })
        .map_err(|err| err.to_string())?;

    Ok(OpState {
        receiver,
        cancel,
        decisions,
    })
}

pub fn trash_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Korobka").join("trash")
}

pub fn new_trash_batch_dir() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    trash_root().join(format!("{stamp}"))
}

pub fn read_trash_manifest(batch: &Path) -> Vec<TrashManifestEntry> {
    let Some(path) = manifest_path(batch) else {
        return Vec::new();
    };
    let Ok(data) = fs::read(crate::path_util::extended(&path)) else {
        return Vec::new();
    };
    serde_json::from_slice::<TrashManifest>(&data)
        .map(|manifest| manifest.entries)
        .unwrap_or_default()
}

fn manifest_path(batch: &Path) -> Option<PathBuf> {
    let name = batch.file_name()?.to_string_lossy();
    Some(batch.with_file_name(format!(".{name}.korobka-trash.json")))
}

pub fn write_trash_manifest(batch: &Path, completed: &[(PathBuf, PathBuf)]) {
    let Some(path) = manifest_path(batch) else {
        return;
    };
    let manifest = TrashManifest {
        entries: completed
            .iter()
            .map(|(source, target)| TrashManifestEntry {
                original: source.clone(),
                trashed: target.clone(),
            })
            .collect(),
    };
    if let Ok(data) = serde_json::to_vec(&manifest) {
        let _ = fs::write(crate::path_util::extended(&path), data);
    }
}

pub fn cleanup_trash_batch(batch: &Path) {
    let empty = fs::read_dir(crate::path_util::extended(batch))
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true);
    if !empty {
        return;
    }
    if let Some(path) = manifest_path(batch) {
        let _ = fs::remove_file(crate::path_util::extended(&path));
    }
    let _ = fs::remove_dir(crate::path_util::extended(batch));
}

pub fn move_item(source: &Path, target: &Path) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(crate::path_util::extended(parent))?;
    }
    match fs::rename(
        crate::path_util::extended(source),
        crate::path_util::extended(target),
    ) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_recursively(source, target)?;
            remove_any(source)
        }
    }
}

pub fn remove_any(path: &Path) -> io::Result<()> {
    let extended = crate::path_util::extended(path);
    let meta = fs::symlink_metadata(&extended)?;
    if meta.is_dir() {
        if is_link_like(&meta) {
            fs::remove_dir(&extended)
        } else {
            fs::remove_dir_all(&extended)
        }
    } else {
        fs::remove_file(&extended)
    }
}

#[cfg(windows)]
fn is_link_like(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x0000_0400 != 0
}

#[cfg(not(windows))]
fn is_link_like(_meta: &fs::Metadata) -> bool {
    false
}

fn run(
    kind: OpKind,
    sources: Vec<PathBuf>,
    destination: PathBuf,
    cancel: Arc<AtomicBool>,
    sender: SyncSender<OpMessage>,
    decisions: Receiver<ConflictDecision>,
) {
    let started = Instant::now();
    let total = sources.len();
    let mut completed = Vec::new();
    let mut skipped: Vec<(PathBuf, String)> = Vec::new();
    let mut replaced = false;
    let mut policy: Option<ConflictChoice> = None;

    if matches!(kind, OpKind::Trash) {
        if let Err(err) = fs::create_dir_all(crate::path_util::extended(&destination)) {
            let _ = sender.send(OpMessage::Failed(localized(
                format!("Не удалось создать корзину: {err}"),
                format!("Could not create trash: {err}"),
            )));
            return;
        }
    }

    for (index, source) in sources.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if sender
            .send(OpMessage::Progress {
                done: index,
                total,
                current: source.clone(),
            })
            .is_err()
        {
            return;
        }

        let name = source
            .file_name()
            .map(|name| name.to_owned())
            .unwrap_or_else(|| "item".into());

        if matches!(kind, OpKind::Copy | OpKind::Move | OpKind::Trash)
            && destination.starts_with(&source)
        {
            skipped.push((
                source.clone(),
                crate::i18n::tr("нельзя переместить папку внутрь самой себя").to_owned(),
            ));
            continue;
        }

        let result = match kind {
            OpKind::Copy | OpKind::Move | OpKind::Trash => {
                let mut target = destination.join(&name);
                if matches!(kind, OpKind::Trash) {
                    target = unique_path(&target);
                }
                if target == source {
                    if kind == OpKind::Copy {
                        target = unique_path(&target);
                    } else {
                        skipped.push((
                            source,
                            crate::i18n::tr("источник и цель совпадают").to_owned(),
                        ));
                        continue;
                    }
                }
                if target.exists() {
                    let choice = match policy {
                        Some(choice) => choice,
                        None => {
                            if sender
                                .send(OpMessage::Conflict {
                                    source: source.clone(),
                                    target: target.clone(),
                                })
                                .is_err()
                            {
                                return;
                            }
                            match decisions.recv() {
                                Ok(decision) => {
                                    if decision.apply_to_all {
                                        policy = Some(decision.choice);
                                    }
                                    decision.choice
                                }
                                Err(_) => return,
                            }
                        }
                    };
                    match choice {
                        ConflictChoice::Skip => {
                            skipped.push((source, crate::i18n::tr("пропущено").to_owned()));
                            continue;
                        }
                        ConflictChoice::Cancel => {
                            cancel.store(true, Ordering::Relaxed);
                            break;
                        }
                        ConflictChoice::KeepBoth => {
                            target = unique_path(&target);
                        }
                        ConflictChoice::Replace => match replace_existing(&source, &target, kind) {
                            Ok(()) => {
                                completed.push((source, target));
                                replaced = true;
                                continue;
                            }
                            Err(err) => {
                                skipped.push((
                                    source,
                                    localized(
                                        format!("не удалось заменить {}: {err}", target.display()),
                                        format!("could not replace {}: {err}", target.display()),
                                    ),
                                ));
                                continue;
                            }
                        },
                    }
                }
                match kind {
                    OpKind::Copy => copy_recursively(&source, &target)
                        .map(|_| target)
                        .map_err(|err| err.to_string()),
                    _ => move_item(&source, &target)
                        .map(|_| target)
                        .map_err(|err| err.to_string()),
                }
            }
            OpKind::Delete => remove_any(&source)
                .map(|_| source.clone())
                .map_err(|err| err.to_string()),
        };

        match result {
            Ok(target) => completed.push((source, target)),
            Err(err) => skipped.push((source, err)),
        }
    }

    if kind == OpKind::Trash && !completed.is_empty() {
        write_trash_manifest(&destination, &completed);
    }

    let _ = sender.send(OpMessage::Finished {
        kind,
        completed,
        skipped,
        replaced,
        elapsed: started.elapsed(),
    });
}

fn replace_existing(source: &Path, target: &Path, kind: OpKind) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let backup = unique_path(&target.with_file_name(format!(".korobka-replace-{stamp}")));
    fs::rename(
        crate::path_util::extended(target),
        crate::path_util::extended(&backup),
    )?;

    let result = match kind {
        OpKind::Copy => copy_recursively(source, target),
        _ => move_item(source, target),
    };

    match result {
        Ok(()) => {
            let _ = remove_any(&backup);
            Ok(())
        }
        Err(err) => {
            let _ = remove_any(target);
            if let Err(restore_err) = fs::rename(
                crate::path_util::extended(&backup),
                crate::path_util::extended(target),
            ) {
                return Err(io::Error::new(
                    err.kind(),
                    localized(
                        format!(
                            "{err}; не удалось восстановить {}: {restore_err}",
                            backup.display()
                        ),
                        format!(
                            "{err}; could not restore {}: {restore_err}",
                            backup.display()
                        ),
                    ),
                ));
            }
            Err(err)
        }
    }
}

pub fn available_path(path: &Path) -> PathBuf {
    unique_path(path)
}

fn unique_path(path: &Path) -> PathBuf {
    if !crate::path_util::extended(path).exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_owned());
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());

    for counter in 2..10_000 {
        let name = match &extension {
            Some(extension) => format!("{stem} ({counter}).{extension}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = parent.join(name);
        if !crate::path_util::extended(&candidate).exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn copy_recursively(source: &Path, target: &Path) -> io::Result<()> {
    let source = crate::path_util::extended(source);
    let target = crate::path_util::extended(target);
    let meta = fs::symlink_metadata(&source)?;
    if meta.is_dir() {
        if is_link_like(&meta) {
            return Ok(());
        }
        fs::create_dir_all(&target)?;
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            copy_recursively(&entry.path(), &target.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(&source, &target).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;

    fn temp_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "korobka_ops_{name}_{}_{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    type OpResult = (Vec<(PathBuf, PathBuf)>, Vec<(PathBuf, String)>);

    fn drive(state: OpState, decisions: Vec<ConflictChoice>) -> OpResult {
        let mut decisions = decisions.into_iter();
        loop {
            match state.receiver.recv_timeout(Duration::from_secs(10)) {
                Ok(OpMessage::Finished {
                    completed, skipped, ..
                }) => return (completed, skipped),
                Ok(OpMessage::Conflict { .. }) => {
                    let choice = decisions.next().unwrap_or(ConflictChoice::Replace);
                    state
                        .decisions
                        .send(ConflictDecision {
                            choice,
                            apply_to_all: true,
                        })
                        .unwrap();
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => panic!("operation timed out"),
                Err(RecvTimeoutError::Disconnected) => panic!("operation disconnected"),
            }
        }
    }

    #[test]
    fn copies_files_and_folders() {
        let temp = temp_dir("copy");
        let source = temp.join("source");
        let destination = temp.join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("a.txt"), b"alpha").unwrap();
        fs::write(source.join("nested").join("b.txt"), b"beta").unwrap();
        fs::create_dir_all(&destination).unwrap();

        let state = start(OpKind::Copy, vec![source.clone()], destination.clone()).unwrap();
        let (completed, skipped) = drive(state, vec![]);
        assert!(skipped.is_empty());
        assert_eq!(completed.len(), 1);
        assert_eq!(
            fs::read(destination.join("source").join("a.txt")).unwrap(),
            b"alpha"
        );
        assert_eq!(
            fs::read(destination.join("source").join("nested").join("b.txt")).unwrap(),
            b"beta"
        );
        assert!(source.join("a.txt").exists());

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn moves_and_replaces_existing_file() {
        let temp = temp_dir("move");
        let source = temp.join("source.txt");
        let destination = temp.join("destination");
        fs::write(&source, b"new").unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("source.txt"), b"old").unwrap();

        let state = start(
            OpKind::Move,
            vec![source.clone()],
            destination.clone(),
        )
        .unwrap();
        let (completed, skipped) = drive(state, vec![ConflictChoice::Replace]);
        assert!(skipped.is_empty());
        assert_eq!(completed.len(), 1);
        assert!(!source.exists());
        assert_eq!(
            fs::read(destination.join("source.txt")).unwrap(),
            b"new"
        );

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn trashes_and_deletes_items() {
        let temp = temp_dir("delete");
        let trash = temp.join("trash");
        let first = temp.join("first.txt");
        let second = temp.join("second");
        fs::write(&first, b"first").unwrap();
        fs::create_dir_all(second.join("nested")).unwrap();
        fs::write(second.join("nested").join("child.txt"), b"child").unwrap();

        let state = start(OpKind::Trash, vec![first.clone()], trash.clone()).unwrap();
        let (completed, skipped) = drive(state, vec![]);
        assert!(skipped.is_empty());
        assert_eq!(completed.len(), 1);
        assert!(!first.exists());
        assert!(trash.join("first.txt").exists());
        let manifest = read_trash_manifest(&trash);
        assert!(manifest.iter().any(|entry| entry.original == first));

        let state = start(OpKind::Delete, vec![second.clone()], temp.clone()).unwrap();
        let (completed, skipped) = drive(state, vec![]);
        assert!(skipped.is_empty());
        assert_eq!(completed.len(), 1);
        assert!(!second.exists());

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn copies_and_deletes_beyond_max_path() {
        let temp = temp_dir("long");
        let mut long = temp.clone();
        for _ in 0..8 {
            long.push("d".repeat(40));
        }
        fs::create_dir_all(crate::path_util::extended(&long)).unwrap();
        let source = long.join("long.txt");
        fs::write(crate::path_util::extended(&source), b"long").unwrap();
        assert!(source.to_string_lossy().len() > 260);

        let destination = temp.join("destination");
        fs::create_dir_all(&destination).unwrap();
        let state = start(OpKind::Copy, vec![source.clone()], destination.clone()).unwrap();
        let (completed, skipped) = drive(state, vec![]);
        assert!(skipped.is_empty());
        assert_eq!(completed.len(), 1);
        assert_eq!(
            fs::read(crate::path_util::extended(&destination.join("long.txt"))).unwrap(),
            b"long"
        );

        let state = start(OpKind::Delete, vec![source.clone()], temp.clone()).unwrap();
        let (completed, skipped) = drive(state, vec![]);
        assert!(skipped.is_empty());
        assert_eq!(completed.len(), 1);
        assert!(!crate::path_util::extended(&source).exists());

        fs::remove_dir_all(crate::path_util::extended(&temp)).unwrap();
    }
}
