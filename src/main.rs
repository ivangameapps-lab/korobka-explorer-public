#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
        Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod archive;
mod drives;
mod fs_ops;
mod i18n;
mod locks;
mod path_util;
mod search;
mod shell_clipboard;

use drives::DriveInfo;
use fs_ops::{ConflictChoice, ConflictDecision, OpKind, OpMessage, OpState};
use i18n::{set_english, system_is_english, tr};
use search::{SearchHit, SearchMessage, SearchMode, SearchQuery};

const MAX_VISIBLE_RESULTS: usize = 100_000;
const MAX_UNDO_STEPS: usize = 50;

const BG: egui::Color32 = egui::Color32::from_rgb(0x0D, 0x11, 0x17);
const SELECTED: egui::Color32 = egui::Color32::from_rgb(0x19, 0x33, 0x4B);
const BORDER_SOFT: egui::Color32 = egui::Color32::from_rgb(0x20, 0x2B, 0x38);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xE6, 0xED, 0xF5);
const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(0x9A, 0xA9, 0xBA);
const TEXT_FAINT: egui::Color32 = egui::Color32::from_rgb(0x73, 0x82, 0x97);
const TEXT_DISABLED: egui::Color32 = egui::Color32::from_rgb(0x53, 0x61, 0x74);
const SUCCESS: egui::Color32 = egui::Color32::from_rgb(0x57, 0xC9, 0x94);
const WARNING: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xB8, 0x66);
const MATTE_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(14, 21, 34, 205);
const MATTE_STRONG: egui::Color32 = egui::Color32::from_rgba_premultiplied(18, 26, 42, 232);
const MATTE_SOFT: egui::Color32 = egui::Color32::from_rgba_premultiplied(12, 17, 27, 178);
const MATTE_BORDER: egui::Color32 = egui::Color32::from_rgba_premultiplied(26, 26, 26, 26);
const GLASS_HIGHLIGHT: egui::Color32 = egui::Color32::from_rgba_premultiplied(14, 14, 14, 14);
/// Заметная, но мягкая полоска для чётных строк таблиц.
const TABLE_STRIPE: egui::Color32 = egui::Color32::from_rgba_premultiplied(14, 18, 24, 44);
const DIR_CACHE_TTL: Duration = Duration::from_secs(30);

static ACCENT_RGB: AtomicU32 = AtomicU32::new(0x004D_A3E8);
static REAL_GLASS: AtomicBool = AtomicBool::new(false);
/// 0 — почти матовое стекло, 100 — максимально прозрачное.
static GLASS_TRANSPARENCY: AtomicU8 = AtomicU8::new(65);

fn accent_rgb() -> [u8; 3] {
    let value = ACCENT_RGB.load(Ordering::Relaxed);
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    ]
}

fn set_accent(rgb: [u8; 3]) {
    ACCENT_RGB.store(
        ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32,
        Ordering::Relaxed,
    );
}

fn mix_channel(from: u8, to: u8, amount: f32) -> u8 {
    (from as f32 + (to as f32 - from as f32) * amount).clamp(0.0, 255.0) as u8
}

fn accent() -> egui::Color32 {
    let [r, g, b] = accent_rgb();
    egui::Color32::from_rgb(r, g, b)
}

fn accent_bright() -> egui::Color32 {
    let [r, g, b] = accent_rgb();
    egui::Color32::from_rgb(
        mix_channel(r, 255, 0.28),
        mix_channel(g, 255, 0.28),
        mix_channel(b, 255, 0.28),
    )
}

fn accent_dim() -> egui::Color32 {
    let [r, g, b] = accent_rgb();
    egui::Color32::from_rgb(
        mix_channel(r, 0, 0.45),
        mix_channel(g, 0, 0.45),
        mix_channel(b, 0, 0.45),
    )
}

fn glass_accent() -> egui::Color32 {
    let [r, g, b] = accent_rgb();
    egui::Color32::from_rgba_unmultiplied(r, g, b, 70)
}

fn set_real_glass(enabled: bool) {
    REAL_GLASS.store(enabled, Ordering::Relaxed);
}

fn real_glass() -> bool {
    REAL_GLASS.load(Ordering::Relaxed)
}

fn set_glass_transparency(value: u8) {
    GLASS_TRANSPARENCY.store(value.min(100), Ordering::Relaxed);
}

fn glass_alpha(min: f32, max: f32) -> u8 {
    let transparency = GLASS_TRANSPARENCY.load(Ordering::Relaxed) as f32 / 100.0;
    (min + (max - min) * (1.0 - transparency))
        .round()
        .clamp(0.0, 255.0) as u8
}

fn glass_fill() -> egui::Color32 {
    if real_glass() {
        egui::Color32::from_rgba_premultiplied(8, 12, 22, glass_alpha(38.0, 200.0))
    } else {
        MATTE_FILL
    }
}

fn glass_strong() -> egui::Color32 {
    if real_glass() {
        egui::Color32::from_rgba_premultiplied(12, 18, 30, glass_alpha(55.0, 230.0))
    } else {
        MATTE_STRONG
    }
}

fn glass_soft() -> egui::Color32 {
    if real_glass() {
        egui::Color32::from_rgba_premultiplied(6, 9, 18, glass_alpha(28.0, 170.0))
    } else {
        MATTE_SOFT
    }
}

fn glass_border() -> egui::Color32 {
    if real_glass() {
        egui::Color32::from_rgba_unmultiplied(120, 150, 190, glass_alpha(34.0, 80.0))
    } else {
        MATTE_BORDER
    }
}

fn glass_input_bg() -> egui::Color32 {
    if real_glass() {
        egui::Color32::from_rgba_unmultiplied(5, 9, 17, glass_alpha(65.0, 200.0))
    } else {
        egui::Color32::from_rgba_unmultiplied(5, 9, 17, 210)
    }
}

fn glass_bg_alpha() -> u8 {
    glass_alpha(3.0, 75.0)
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Icon {
    Back,
    Forward,
    Up,
    Refresh,
    Search,
    List,
    Grid,
    Sidebar,
    Plus,
    Close,
    Folder,
    File,
    Drive,
    More,
    Properties,
    Copy,
    Cut,
    Paste,
    Rename,
    Trash,
    Undo,
    ChevronDown,
    ChevronRight,
    Edit,
    Computer,
    Download,
    Image,
    Music,
    Video,
    Desktop,
}

#[derive(Clone)]
struct FsEntry {
    path: PathBuf,
    name: String,
    name_lower: String,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<SystemTime>,
    size_text: OnceLock<String>,
    modified_text: OnceLock<String>,
    kind: &'static str,
    color: egui::Color32,
}

impl FsEntry {
    fn size_text(&self) -> &str {
        self.size_text
            .get_or_init(|| self.size.map(format_size).unwrap_or_default())
    }
}

#[derive(Clone)]
struct TrashItem {
    path: PathBuf,
    original: Option<PathBuf>,
    batch: PathBuf,
    name: String,
    is_dir: bool,
    deleted: Option<SystemTime>,
}

#[derive(Clone)]
struct ArchiveEntry {
    inner: String,
    name: String,
    is_dir: bool,
    size: Option<u64>,
    index: Option<usize>,
    path: PathBuf,
    kind: &'static str,
    color: egui::Color32,
}

impl ArchiveEntry {
    fn size_text(&self) -> String {
        self.size.map(format_size).unwrap_or_default()
    }
}

struct ArchiveView {
    zip_path: PathBuf,
    inner: String,
    entries: Vec<ArchiveEntry>,
    filter: String,
    selected: Option<String>,
    history: Vec<String>,
    history_pos: usize,
    error: Option<String>,
}

impl ArchiveView {
    fn new(zip_path: PathBuf) -> Self {
        let mut view = Self {
            zip_path,
            inner: String::new(),
            entries: Vec::new(),
            filter: String::new(),
            selected: None,
            history: vec![String::new()],
            history_pos: 0,
            error: None,
        };
        view.reload();
        view
    }

    fn reload(&mut self) {
        match archive::list(&self.zip_path, &self.inner) {
            Ok(entries) => {
                self.entries = entries
                    .into_iter()
                    .map(|entry| {
                        let path = self
                            .zip_path
                            .join(entry.inner.replace('/', std::path::MAIN_SEPARATOR_STR));
                        let color = category_color(&path, entry.is_dir);
                        ArchiveEntry {
                            kind: kind_label(&path, entry.is_dir),
                            inner: entry.inner,
                            name: entry.name,
                            is_dir: entry.is_dir,
                            size: entry.size,
                            index: entry.index,
                            path,
                            color,
                        }
                    })
                    .collect();
                self.error = None;
            }
            Err(error) => {
                self.entries.clear();
                self.error = Some(tr_msg(
                format!("Не удалось прочитать ZIP: {error}"),
                format!("Could not read ZIP: {error}"),
            ));
            }
        }
    }

    fn open_dir(&mut self, inner: String) {
        let inner = normalize_archive_inner(&inner);
        if inner == self.inner {
            return;
        }
        self.history.truncate(self.history_pos + 1);
        self.history.push(inner.clone());
        self.history_pos = self.history.len() - 1;
        self.inner = inner;
        self.selected = None;
        self.reload();
    }

    fn back(&mut self) -> bool {
        if self.history_pos == 0 {
            return false;
        }
        self.history_pos -= 1;
        self.inner = self.history[self.history_pos].clone();
        self.selected = None;
        self.reload();
        true
    }

    fn forward(&mut self) -> bool {
        if self.history_pos + 1 >= self.history.len() {
            return false;
        }
        self.history_pos += 1;
        self.inner = self.history[self.history_pos].clone();
        self.selected = None;
        self.reload();
        true
    }

    fn up(&mut self) -> bool {
        if self.inner.is_empty() {
            return false;
        }
        let parent = self
            .inner
            .trim_end_matches('/')
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/"))
            .unwrap_or_default();
        self.open_dir(parent);
        true
    }

    fn display_path(&self) -> String {
        let mut path = self.zip_path.display().to_string();
        if !self.inner.is_empty() {
            path.push('\\');
            path.push_str(self.inner.trim_end_matches('/').replace('/', "\\").as_str());
        }
        path
    }

    fn find(&self, inner: &str) -> Option<ArchiveEntry> {
        self.entries
            .iter()
            .find(|entry| entry.inner == inner)
            .cloned()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct SessionState {
    tabs: Vec<PathBuf>,
    active_tab: usize,
    view: u8,
    sort_key: u8,
    sort_ascending: bool,
    sidebar_width: f32,
    sidebar_collapsed: bool,
    compact_rows: bool,
    appearance: u8,
    accent: [u8; 3],
    language: u8,
    glass_blur: bool,
    glass_transparency: u8,
    folders_first: bool,
    show_hidden: bool,
    striped_rows: bool,
    ui_scale: f32,
    animations: bool,
    confirm_permanent_delete: bool,
    search_mode_index: usize,
    search_threads: u8,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active_tab: 0,
            view: 0,
            sort_key: 0,
            sort_ascending: true,
            sidebar_width: 176.0,
            sidebar_collapsed: false,
            compact_rows: false,
            appearance: 0,
            accent: [0x4D, 0xA3, 0xE8],
            language: 0,
            glass_blur: true,
            glass_transparency: 65,
            folders_first: true,
            show_hidden: true,
            striped_rows: false,
            ui_scale: 1.0,
            animations: true,
            confirm_permanent_delete: true,
            search_mode_index: 1,
            search_threads: 8,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SortState {
    key: SortKey,
    ascending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            ascending: true,
        }
    }
}

/// Область поиска: текущая папка с подпапками или весь том.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchScope {
    Folder,
    Volume,
}

struct SearchState {
    receiver: Receiver<SearchMessage>,
    cancel: Arc<AtomicBool>,
    scanned: u64,
    scope: SearchScope,
}

struct DirLoad {
    receiver: Receiver<DirLoadResult>,
}

struct DirLoadResult {
    path: PathBuf,
    entries: Vec<FsEntry>,
    error: Option<String>,
    sort: SortState,
    elapsed: Duration,
}

trait PathItem {
    fn path(&self) -> &Path;
}

impl PathItem for FsEntry {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl PathItem for SearchHit {
    fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipMode {
    Copy,
    Move,
}

#[derive(Clone)]
struct Clipboard {
    paths: Vec<PathBuf>,
    mode: ClipMode,
}

struct OpConflict {
    source: PathBuf,
    target: PathBuf,
    sender: SyncSender<ConflictDecision>,
}

struct RenameDialog {
    path: PathBuf,
    name: String,
    focus: bool,
}

struct LockFlow {
    receiver: Receiver<LockMessage>,
}

enum LockMessage {
    Checked {
        kind: OpKind,
        paths: Vec<PathBuf>,
        scan: Result<locks::LockScan, String>,
    },
    Closed {
        kind: OpKind,
        paths: Vec<PathBuf>,
        result: Result<(), String>,
    },
}

struct LockDialog {
    kind: OpKind,
    paths: Vec<PathBuf>,
    blocked_paths: Vec<PathBuf>,
    processes: Vec<locks::LockingProcess>,
    selected: BTreeSet<u32>,
    busy: bool,
    error: String,
}

enum UndoAction {
    MoveBack(Vec<(PathBuf, PathBuf)>),
    RemoveCreated(Vec<PathBuf>),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ViewMode {
    #[default]
    List,
    Tiles,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Appearance {
    #[default]
    Matte,
    Glass,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Language {
    #[default]
    System,
    Russian,
    English,
}

fn language_is_english(language: Language) -> bool {
    match language {
        Language::Russian => false,
        Language::English => true,
        Language::System => system_is_english(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CreateKind {
    Folder,
    Text,
    Word,
    Excel,
    Zip,
    EmptyFile,
}

#[derive(Clone)]
struct Tab {
    dir: PathBuf,
    history: Vec<PathBuf>,
    history_pos: usize,
    filter_text: String,
    view: ViewMode,
    selected: Option<PathBuf>,
    selection: BTreeSet<PathBuf>,
    anchor: Option<PathBuf>,
}

struct KorobkaApp {
    current_dir: PathBuf,
    path_text: String,
    entries: Vec<FsEntry>,
    filter_text: String,
    filter_indices: Vec<usize>,
    filter_dirty: bool,
    scroll_to_row: Option<usize>,
    dir_load: Option<DirLoad>,
    dir_cache: Vec<(PathBuf, Instant, Vec<FsEntry>)>,
    selected: Option<PathBuf>,
    selection: BTreeSet<PathBuf>,
    anchor: Option<PathBuf>,
    status: String,

    search_mode_index: usize,
    search_threads: u8,
    search_results: Vec<SearchHit>,
    search_state: Option<SearchState>,
    last_search_scope: Option<SearchScope>,
    search_summary: String,
    archive: Option<ArchiveView>,

    clipboard: Option<Clipboard>,
    rename: Option<RenameDialog>,
    lock_flow: Option<LockFlow>,
    lock_dialog: Option<LockDialog>,
    op_state: Option<OpState>,
    op_conflict: Option<OpConflict>,
    op_apply_all: bool,
    op_progress: String,
    undo_stack: Vec<UndoAction>,

    picker_open: bool,
    picker_dir: PathBuf,
    picker_path_text: String,
    picker_dirs: Vec<PathBuf>,
    picker_error: String,
    drives: Vec<DriveInfo>,
    drives_loaded: bool,

    sort: SortState,
    history: Vec<PathBuf>,
    history_pos: usize,
    places: Vec<(String, PathBuf)>,
    style_ready: bool,
    tabs: Vec<Tab>,
    active_tab: usize,
    view: ViewMode,
    sidebar_collapsed: bool,
    sidebar_width: f32,
    show_properties: bool,
    address_editing: bool,
    focus_address: bool,
    focus_search: bool,
    background_menu: Option<(egui::Pos2, Instant)>,
    settings_open: bool,
    settings_save_at: Option<Instant>,
    trash_open: bool,
    trash_items: Vec<TrashItem>,
    trash_selected: Option<PathBuf>,
    trash_error: String,
    trash_delete_pending: Option<Vec<PathBuf>>,
    compact_rows: bool,
    appearance: Appearance,
    accent: [u8; 3],
    language: Language,
    glass_blur: bool,
    glass_transparency: u8,
    folders_first: bool,
    show_hidden: bool,
    striped_rows: bool,
    ui_scale: f32,
    animations: bool,
    confirm_permanent_delete: bool,
    pending_permanent_delete: Option<Vec<PathBuf>>,
    backdrop_ready: bool,
    backdrop_attempted: bool,
}

impl KorobkaApp {
    fn new() -> Self {
        let mut app = Self::default();
        let session = load_session();
        let mut restored_tabs = Vec::new();
        let mut restored_active = 0;
        if let Some(session) = session {
            app.view = if session.view == 1 {
                ViewMode::Tiles
            } else {
                ViewMode::List
            };
            app.sort = SortState {
                key: match session.sort_key {
                    1 => SortKey::Size,
                    2 => SortKey::Modified,
                    3 => SortKey::Kind,
                    _ => SortKey::Name,
                },
                ascending: session.sort_ascending,
            };
            if session.sidebar_width > 0.0 {
                app.sidebar_width = session.sidebar_width.clamp(148.0, 240.0);
            }
            app.sidebar_collapsed = session.sidebar_collapsed;
            app.compact_rows = session.compact_rows;
            app.appearance = if session.appearance == 1 {
                Appearance::Glass
            } else {
                Appearance::Matte
            };
            app.accent = session.accent;
            app.language = match session.language {
                1 => Language::Russian,
                2 => Language::English,
                _ => Language::System,
            };
            app.glass_blur = session.glass_blur;
            app.glass_transparency = session.glass_transparency.min(100);
            app.folders_first = session.folders_first;
            app.show_hidden = session.show_hidden;
            app.striped_rows = session.striped_rows;
            app.ui_scale = session.ui_scale.clamp(0.8, 1.35);
            app.animations = session.animations;
            app.confirm_permanent_delete = session.confirm_permanent_delete;
            app.search_mode_index = session.search_mode_index.min(1);
            app.search_threads = session.search_threads.clamp(2, 16);
            restored_tabs = session
                .tabs
                .into_iter()
                .filter(|path| path_is_dir(path))
                .collect();
            restored_active = session.active_tab;
        }
        if restored_tabs.is_empty() {
            if let Some(arg) = std::env::args_os().nth(1) {
                let path = PathBuf::from(arg);
                if path_is_dir(&path) {
                    app.current_dir = path;
                }
            }
            restored_tabs.push(app.current_dir.clone());
            restored_active = 0;
        } else {
            restored_active = restored_active.min(restored_tabs.len() - 1);
            app.current_dir = restored_tabs[restored_active].clone();
        }
        app.path_text = app.current_dir.display().to_string();
        app.history = vec![app.current_dir.clone()];
        app.history_pos = 0;
        app.tabs = restored_tabs
            .into_iter()
            .map(|dir| Tab {
                history: vec![dir.clone()],
                dir,
                history_pos: 0,
                filter_text: String::new(),
                view: app.view,
                selected: None,
                selection: BTreeSet::new(),
                anchor: None,
            })
            .collect();
        app.active_tab = restored_active;
        set_english(language_is_english(app.language));
        app.places = quick_places();
        app.status = tr("Готово").to_owned();
        app.load_directory();
        app
    }

    fn load_directory(&mut self) {
        let path = self.current_dir.clone();
        self.start_dir_load(path, false);
    }

    fn refresh_directory(&mut self) {
        self.dir_cache.clear();
        let path = self.current_dir.clone();
        self.start_dir_load(path, false);
    }

    fn start_dir_load(&mut self, path: PathBuf, use_cache: bool) {
        if !path_is_dir(&path) {
            self.status = tr_msg(
                format!("Папка не найдена: {}", path.display()),
                format!("Folder not found: {}", path.display()),
            );
            return;
        }
        self.archive = None;
        if path != self.current_dir {
            self.clear_filter();
        }
        self.dir_load = None;
        self.current_dir = path.clone();
        self.path_text = path.display().to_string();
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.dir = path.clone();
        }
        self.selected = None;
        self.selection.clear();
        self.anchor = None;

        if use_cache {
            if let Some(mut entries) = self.take_cached_dir(&path) {
                sort_fs_entries(&mut entries, self.sort, self.folders_first);
                self.entries = entries;
                self.filter_dirty = true;
                // A cached directory is installed synchronously during a
                // sidebar click, after the per-frame filter rebuild. Rebuild
                // here so the table never sees indices from the previous disk.
                self.rebuild_filter();
                self.status = tr("Готово").to_owned();
                return;
            }
        }

        self.entries.clear();
        self.filter_indices.clear();
        self.filter_dirty = false;
        self.status = tr("Загрузка…").to_owned();
        let sort = self.sort;
        let folders_first = self.folders_first;
        let show_hidden = self.show_hidden;
        let (tx, rx) = mpsc::channel();
        let spawn_path = path.clone();
        let spawned = thread::Builder::new()
            .name("korobka-dir".to_owned())
            .spawn(move || {
                let started = Instant::now();
                let (entries, error) = match read_directory(&spawn_path, show_hidden) {
                    Ok(mut entries) => {
                        sort_fs_entries(&mut entries, sort, folders_first);
                        (entries, None)
                    }
                    Err(err) => (Vec::new(), Some(err.to_string())),
                };
                let _ = tx.send(DirLoadResult {
                    path: spawn_path,
                    entries,
                    error,
                    sort,
                    elapsed: started.elapsed(),
                });
            });
        match spawned {
            Ok(_) => self.dir_load = Some(DirLoad { receiver: rx }),
            Err(err) => self.status = tr_msg(
                format!("Не удалось запустить чтение папки: {err}"),
                format!("Could not start reading folder: {err}"),
            ),
        }
    }

    fn take_cached_dir(&mut self, path: &Path) -> Option<Vec<FsEntry>> {
        let position = self
            .dir_cache
            .iter()
            .position(|(cached, _, _)| cached == path)?;
        let (_, at, entries) = self.dir_cache.remove(position);
        if at.elapsed() <= DIR_CACHE_TTL {
            Some(entries)
        } else {
            None
        }
    }

    fn store_current_in_cache(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.dir_cache
            .retain(|(cached, _, _)| cached != &self.current_dir);
        self.dir_cache.push((
            self.current_dir.clone(),
            Instant::now(),
            std::mem::take(&mut self.entries),
        ));
        self.filter_indices.clear();
        while self.dir_cache.len() > 10 {
            self.dir_cache.remove(0);
        }
        let mut total: usize = self
            .dir_cache
            .iter()
            .map(|(_, _, entries)| entries.len())
            .sum();
        while total > 250_000 && self.dir_cache.len() > 1 {
            let (_, _, entries) = self.dir_cache.remove(0);
            total -= entries.len();
        }
    }

    fn sort_entries(&mut self) {
        sort_fs_entries(&mut self.entries, self.sort, self.folders_first);
        self.filter_dirty = true;
    }

    fn rebuild_filter(&mut self) {
        let query = SearchQuery::new(
            &self.filter_text,
            SearchMode::from_index(self.search_mode_index),
        );
        if query.is_empty() {
            self.filter_indices.clear();
            self.filter_indices.extend(0..self.entries.len());
        } else if self.entries.len() >= 20_000 {
            self.filter_indices = self
                .entries
                .par_iter()
                .enumerate()
                .filter(|(_, entry)| filter_matches(&query, entry))
                .map(|(index, _)| index)
                .collect();
        } else {
            self.filter_indices.clear();
            self.filter_indices.reserve(self.entries.len());
            self.filter_indices.extend(
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| filter_matches(&query, entry))
                    .map(|(index, _)| index),
            );
        }
        self.filter_dirty = false;
    }

    fn clear_filter(&mut self) {
        if !self.filter_text.is_empty() {
            self.filter_text.clear();
            self.filter_dirty = true;
        }
    }

    fn open_history(&mut self, path: PathBuf) {
        self.clear_search();
        self.store_current_in_cache();
        self.start_dir_load(path, true);
    }

    fn navigate_to(&mut self, path: PathBuf) {
        if archive::is_zip(&path) && path.is_file() {
            self.open_archive(path);
            return;
        }
        if !path_is_dir(&path) {
            self.status = tr("Это не папка").to_owned();
            return;
        }
        self.address_editing = false;
        self.background_menu = None;
        self.pending_permanent_delete = None;
        self.clear_search();
        self.store_current_in_cache();
        self.start_dir_load(path.clone(), true);
        if self.current_dir == path {
            self.history.truncate(self.history_pos + 1);
            if self.history.last() != Some(&path) {
                self.history.push(path);
            }
            self.history_pos = self.history.len().saturating_sub(1);
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            self.navigate_to(parent.to_path_buf());
        }
    }

    fn open_path_item(&mut self, path: PathBuf, is_dir: bool) {
        if is_dir {
            self.navigate_to(path);
        } else if archive::is_zip(&path) {
            self.open_archive(path);
        } else if let Err(error) = open::that(&path) {
            self.status = tr_msg(
                format!("Не удалось открыть: {error}"),
                format!("Could not open: {error}"),
            );
        }
    }

    fn open_archive(&mut self, zip_path: PathBuf) {
        self.clear_search();
        self.background_menu = None;
        self.address_editing = false;
        let view = ArchiveView::new(zip_path);
        self.path_text = view.display_path();
        self.status = view
            .error
            .clone()
            .unwrap_or_else(|| format!("ZIP: {}", items_text(view.entries.len())));
        self.archive = Some(view);
    }

    fn close_archive(&mut self) {
        if self.archive.take().is_some() {
            self.path_text = self.current_dir.display().to_string();
            self.status = tr("Готово").to_owned();
        }
    }

    fn archive_navigate(&mut self, inner: String) {
        let mut path = None;
        if let Some(view) = self.archive.as_mut() {
            view.open_dir(inner);
            path = Some(view.display_path());
        }
        if let Some(path) = path {
            self.path_text = path;
        }
    }

    fn archive_back(&mut self) {
        let mut path = None;
        let mut close = false;
        if let Some(view) = self.archive.as_mut() {
            if view.back() {
                path = Some(view.display_path());
            } else {
                close = true;
            }
        }
        if close {
            self.close_archive();
        } else if let Some(path) = path {
            self.path_text = path;
        }
    }

    fn archive_forward(&mut self) {
        let mut path = None;
        if let Some(view) = self.archive.as_mut() {
            if view.forward() {
                path = Some(view.display_path());
            }
        }
        if let Some(path) = path {
            self.path_text = path;
        }
    }

    fn archive_up(&mut self) {
        let mut path = None;
        let mut close = false;
        if let Some(view) = self.archive.as_mut() {
            if view.up() {
                path = Some(view.display_path());
            } else {
                close = true;
            }
        }
        if close {
            self.close_archive();
        } else if let Some(path) = path {
            self.path_text = path;
        }
    }

    fn archive_reload(&mut self) {
        if let Some(view) = self.archive.as_mut() {
            view.reload();
            self.path_text = view.display_path();
            self.status = view
                .error
                .clone()
                .unwrap_or_else(|| format!("ZIP: {}", items_text(view.entries.len())));
        }
    }

    fn archive_open_selected(&mut self) {
        let Some(inner) = self.archive.as_ref().and_then(|view| view.selected.clone()) else {
            return;
        };
        self.archive_open_inner(inner);
    }

    fn archive_open_inner(&mut self, inner: String) {
        let Some(entry) = self.archive.as_ref().and_then(|view| view.find(&inner)) else {
            return;
        };
        if entry.is_dir {
            self.archive_navigate(entry.inner);
            return;
        }
        let Some(index) = entry.index else {
            self.status = tr("Не удалось найти файл в архиве").to_owned();
            return;
        };
        let Some(zip_path) = self.archive.as_ref().map(|view| view.zip_path.clone()) else {
            return;
        };
        match archive::extract_to_temp(&zip_path, index) {
            Ok(path) => {
                if archive::is_zip(&path) {
                    self.open_archive(path);
                } else if let Err(error) = open::that(&path) {
                    self.status = tr_msg(
                format!("Не удалось открыть: {error}"),
                format!("Could not open: {error}"),
            );
                }
            }
            Err(error) => {
                self.status = tr_msg(
                format!("Не удалось извлечь файл: {error}"),
                format!("Could not extract file: {error}"),
            );
            }
        }
    }

    fn archive_extract_inner(&mut self, inner: &str) {
        let Some(entry) = self.archive.as_ref().and_then(|view| view.find(inner)) else {
            return;
        };
        let Some(index) = entry.index else {
            return;
        };
        let Some(zip_path) = self.archive.as_ref().map(|view| view.zip_path.clone()) else {
            return;
        };
        let destination = archive::unique_extraction_dir(&zip_path);
        match archive::extract_entry(&zip_path, index, &destination) {
            Ok(path) => self.status = tr_msg(
                format!("Извлечено: {}", path.display()),
                format!("Extracted: {}", path.display()),
            ),
            Err(error) => self.status = tr_msg(
                format!("Не удалось извлечь: {error}"),
                format!("Could not extract: {error}"),
            ),
        }
    }

    fn archive_extract_all(&mut self) {
        let Some(zip_path) = self.archive.as_ref().map(|view| view.zip_path.clone()) else {
            return;
        };
        let destination = archive::unique_extraction_dir(&zip_path);
        match archive::extract_all(&zip_path, &destination) {
            Ok(count) => {
                self.status = tr_msg(
            format!("Извлечено объектов: {count} → {}", destination.display()),
            format!("Extracted items: {count} → {}", destination.display()),
        );
            }
            Err(error) => self.status = tr_msg(
            format!("Не удалось извлечь архив: {error}"),
            format!("Could not extract archive: {error}"),
        ),
        }
    }

    fn nav_back(&mut self) {
        if self.archive.is_some() {
            self.archive_back();
        } else {
            self.go_back();
        }
    }

    fn nav_forward(&mut self) {
        if self.archive.is_some() {
            self.archive_forward();
        } else {
            self.go_forward();
        }
    }

    fn nav_up(&mut self) {
        if self.archive.is_some() {
            self.archive_up();
        } else {
            self.go_up();
        }
    }

    fn nav_refresh(&mut self) {
        if self.archive.is_some() {
            self.archive_reload();
        } else {
            self.refresh_directory();
        }
    }

    fn go_back(&mut self) {
        if self.history_pos == 0 || self.history.is_empty() {
            return;
        }
        self.history_pos -= 1;
        let path = self.history[self.history_pos].clone();
        self.open_history(path);
    }

    fn go_forward(&mut self) {
        if self.history_pos + 1 >= self.history.len() {
            return;
        }
        self.history_pos += 1;
        let path = self.history[self.history_pos].clone();
        self.open_history(path);
    }

    fn save_active_tab(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.dir = self.current_dir.clone();
            tab.history = self.history.clone();
            tab.history_pos = self.history_pos;
            tab.filter_text = self.filter_text.clone();
            tab.view = self.view;
            tab.selected = self.selected.clone();
            tab.selection = self.selection.clone();
            tab.anchor = self.anchor.clone();
        }
    }

    fn restore_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active_tab = index;
        let tab = self.tabs[index].clone();
        self.current_dir = tab.dir.clone();
        self.path_text = tab.dir.display().to_string();
        self.history = tab.history;
        if self.history.is_empty() {
            self.history.push(tab.dir.clone());
        }
        self.history_pos = tab.history_pos.min(self.history.len().saturating_sub(1));
        self.filter_text = tab.filter_text;
        self.filter_dirty = true;
        self.view = tab.view;
        self.address_editing = false;
        self.clear_search();
        self.start_dir_load(tab.dir.clone(), true);
        self.selection = tab.selection;
        self.selected = tab.selected;
        self.anchor = tab.anchor;
        self.scroll_to_row = None;
        self.status = tr("Готово").to_owned();
    }

    fn switch_tab(&mut self, index: usize) {
        if index == self.active_tab || index >= self.tabs.len() {
            return;
        }
        self.store_current_in_cache();
        self.save_active_tab();
        self.restore_tab(index);
    }

    fn new_tab(&mut self, path: PathBuf) {
        if !path_is_dir(&path) {
            self.status = tr_msg(
                format!("Папка не найдена: {}", path.display()),
                format!("Folder not found: {}", path.display()),
            );
            return;
        }
        self.store_current_in_cache();
        self.save_active_tab();
        self.tabs.push(Tab {
            dir: path.clone(),
            history: vec![path.clone()],
            history_pos: 0,
            filter_text: String::new(),
            view: self.view,
            selected: None,
            selection: BTreeSet::new(),
            anchor: None,
        });
        self.active_tab = self.tabs.len() - 1;
        self.current_dir = path.clone();
        self.path_text = path.display().to_string();
        self.history = vec![path.clone()];
        self.history_pos = 0;
        self.filter_text.clear();
        self.filter_dirty = true;
        self.selection.clear();
        self.selected = None;
        self.anchor = None;
        self.address_editing = false;
        self.clear_search();
        self.start_dir_load(path, true);
        self.status = tr("Готово").to_owned();
    }

    fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
        }
        self.save_active_tab();
        let was_active = index == self.active_tab;
        self.tabs.remove(index);
        if was_active {
            let target = index.min(self.tabs.len() - 1);
            self.restore_tab(target);
        } else if index < self.active_tab {
            self.active_tab -= 1;
        }
    }

    fn start_search(&mut self, scope: SearchScope) {
        let query = self.filter_text.trim().to_owned();
        if query.is_empty() {
            self.status = if i18n::english() {
                "Enter a search name".to_owned()
            } else {
                "Введи имя для поиска".to_owned()
            };
            return;
        }
        self.archive = None;
        self.abort_search();
        self.background_menu = None;
        self.search_results.clear();
        self.selection.clear();
        self.selected = None;
        self.anchor = None;
        self.search_summary = if i18n::english() {
            match scope {
                SearchScope::Volume => "Searching the whole drive…".to_owned(),
                SearchScope::Folder => "Searching the folder and subfolders…".to_owned(),
            }
        } else {
            match scope {
                SearchScope::Volume => "Ищу на всём диске…".to_owned(),
                SearchScope::Folder => "Ищу в папке и подпапках…".to_owned(),
            }
        };

        // Для обеих областей используется кэш: первый поиск обходит папку или
        // том и пишет индекс, следующие читают его и отвечают за доли секунды.
        let (search_root, cache_path) = match scope {
            SearchScope::Volume => {
                let root = volume_root(&self.current_dir);
                let cache = index_cache_path(&root);
                (root, Some(cache))
            }
            SearchScope::Folder => {
                let root = self.current_dir.clone();
                let cache = index_cache_path(&root);
                (root, Some(cache))
            }
        };
        self.last_search_scope = Some(scope);
        let mode = SearchMode::from_index(self.search_mode_index);
        let search_threads = self.search_threads.clamp(2, 16) as usize;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(search_threads)
            .build()
            .ok();
        let (tx, rx) = mpsc::sync_channel::<SearchMessage>(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let started = Instant::now();

        thread::Builder::new()
            .name("korobka-search".to_owned())
            .spawn(move || {
                if let Some(pool) = pool {
                    pool.install(|| {
                        search::search_worker(
                            search_root,
                            cache_path,
                            query,
                            mode,
                            worker_cancel,
                            tx,
                            started,
                        );
                    });
                } else {
                    search::search_worker(
                        search_root,
                        cache_path,
                        query,
                        mode,
                        worker_cancel,
                        tx,
                        started,
                    );
                }
            })
            .map(|_| {
                self.search_state = Some(SearchState {
                    receiver: rx,
                    cancel,
                    scanned: 0,
                    scope,
                });
            })
            .unwrap_or_else(|err| {
                self.search_summary = tr_msg(
            format!("Не удалось запустить поиск: {err}"),
            format!("Could not start search: {err}"),
        );
            });
    }

    /// Останавливает текущий поиск, не трогая уже найденные результаты.
    fn abort_search(&mut self) {
        if let Some(state) = self.search_state.take() {
            state.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Сбрасывает результаты поиска, оставляя текст в поле: при вводе мы
    /// мгновенно фильтруем текущую папку и не обходим диск на каждый символ.
    fn reset_search_results(&mut self) {
        self.abort_search();
        self.search_results.clear();
        self.search_summary.clear();
        self.last_search_scope = None;
    }

    fn clear_search(&mut self) {
        self.clear_filter();
        self.abort_search();
        self.background_menu = None;
        self.search_results.clear();
        self.search_summary.clear();
        self.last_search_scope = None;
    }

    fn poll_search(&mut self, ctx: &egui::Context) {
        let Some(state) = self.search_state.as_mut() else {
            return;
        };
        let mut messages = 0;
        let mut finished = false;
        // Limit work per frame so thousands of results do not freeze the UI.
        while messages < 256 {
            match state.receiver.try_recv() {
                Ok(SearchMessage::IndexInfo { from_cache }) => {
                    self.search_summary = if i18n::english() {
                        if from_cache {
                            "Reading index…".to_owned()
                        } else {
                            "Building index…".to_owned()
                        }
                    } else if from_cache {
                        "Читаю индекс…".to_owned()
                    } else {
                        "Строю индекс…".to_owned()
                    };
                    messages += 1;
                }
                Ok(SearchMessage::IndexProgress { scanned }) => {
                    state.scanned = scanned;
                    self.search_summary = if i18n::english() {
                        format!("Scanned: {scanned}")
                    } else {
                        format!("Просмотрено объектов: {scanned}")
                    };
                    messages += 1;
                }
                Ok(SearchMessage::Batch { hits, scanned }) => {
                    state.scanned = scanned;
                    let room = MAX_VISIBLE_RESULTS.saturating_sub(self.search_results.len());
                    self.search_results.extend(hits.into_iter().take(room));
                    messages += 1;
                }
                Ok(SearchMessage::Finished { scanned, elapsed }) => {
                    state.scanned = scanned;
                    // Сначала точные имена, затем совпадения с начала имени.
                    self.search_results.sort_unstable_by(SearchHit::rank_cmp);
                    self.search_summary = if i18n::english() {
                        format!(
                            "Done: scanned {scanned} items in {:.2} s. Found: {}{}",
                            elapsed.as_secs_f64(),
                            self.search_results.len(),
                            if self.search_results.len() >= MAX_VISIBLE_RESULTS {
                                " (result limit reached)"
                            } else {
                                ""
                            }
                        )
                    } else {
                        format!(
                            "Готово: просмотрено {scanned} объектов за {:.2} с. Найдено: {}{}",
                            elapsed.as_secs_f64(),
                            self.search_results.len(),
                            if self.search_results.len() >= MAX_VISIBLE_RESULTS {
                                " (показан лимит результатов)"
                            } else {
                                ""
                            }
                        )
                    };
                    finished = true;
                    break;
                }
                Ok(SearchMessage::Failed(error)) => {
                    self.search_summary = if i18n::english() {
                        format!("Search error: {error}")
                    } else {
                        format!("Ошибка поиска: {error}")
                    };
                    finished = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.search_summary = if i18n::english() {
                        format!("Search interrupted. Found: {}", self.search_results.len())
                    } else {
                        format!("Поиск прерван. Найдено: {}", self.search_results.len())
                    };
                    finished = true;
                    break;
                }
            }
        }
        if !finished && self.search_results.len() >= MAX_VISIBLE_RESULTS {
            state.cancel.store(true, Ordering::Relaxed);
            self.search_summary = if i18n::english() {
                format!(
                    "Reached limit of {MAX_VISIBLE_RESULTS} results — search stopped"
                )
            } else {
                format!("Достигнут лимит {MAX_VISIBLE_RESULTS} результатов — поиск остановлен")
            };
            finished = true;
        }
        if finished {
            self.search_state = None;
        } else {
            self.search_summary = if i18n::english() {
                format!(
                    "Searching… scanned: {}, found: {}",
                    state.scanned,
                    self.search_results.len()
                )
            } else {
                format!(
                    "Поиск… просмотрено: {}, найдено: {}",
                    state.scanned,
                    self.search_results.len()
                )
            };
            ctx.request_repaint_after(Duration::from_millis(if messages > 0 { 16 } else { 100 }));
        }
    }

    fn begin_rename(&mut self) {
        if self.selection.len() != 1 {
            self.status = tr("Выберите один объект для переименования").to_owned();
            return;
        }
        let Some(path) = self.selection.iter().next().cloned() else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.rename = Some(RenameDialog {
            path,
            name,
            focus: true,
        });
    }

    fn commit_rename(&mut self) {
        let Some(dialog) = self.rename.take() else {
            return;
        };
        let name = dialog.name.trim().to_owned();
        if name.is_empty() {
            self.status = tr("Имя не может быть пустым").to_owned();
            self.rename = Some(dialog);
            return;
        }
        let new_path = dialog.path.with_file_name(&name);
        if new_path == dialog.path {
            return;
        }
        if path_exists(&new_path) {
            self.status = tr_msg(
            format!("{} уже существует", new_path.display()),
            format!("{} already exists", new_path.display()),
        );
            self.rename = Some(dialog);
            return;
        }
        match fs::rename(
            crate::path_util::extended(&dialog.path),
            crate::path_util::extended(&new_path),
        ) {
            Ok(()) => {
                self.status = if i18n::english() {
            format!("Renamed: {name}")
        } else {
            format!("Переименовано: {name}")
        };
                self.purge_search_results(std::slice::from_ref(&dialog.path));
                self.invalidate_index();
                self.load_directory();
                self.selection.insert(new_path);
            }
            Err(err) => {
                self.status = tr_msg(
            format!("Не удалось переименовать: {err}"),
            format!("Could not rename: {err}"),
        );
                self.rename = Some(dialog);
            }
        }
    }

    fn draw_rename(&mut self, ctx: &egui::Context) {
        if self.rename.is_none() {
            return;
        }
        let mut open = true;
        let mut commit = false;
        let mut cancel = false;

        if let Some(dialog) = self.rename.as_mut() {
            egui::Window::new(tr("Переименовать"))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    let response =
                        ui.add(egui::TextEdit::singleline(&mut dialog.name).desired_width(360.0));
                    if std::mem::take(&mut dialog.focus) {
                        response.request_focus();
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked()
                            || (response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                        {
                            commit = true;
                        }
                        if ui.button(tr("Отмена")).clicked()
                            || ui.input(|input| input.key_pressed(egui::Key::Escape))
                        {
                            cancel = true;
                        }
                    });
                });
        }

        if commit {
            self.commit_rename();
            release_text_focus(ctx);
        } else if cancel || !open {
            self.rename = None;
            release_text_focus(ctx);
        }
    }

    fn draw_confirm_permanent_delete(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.pending_permanent_delete.clone() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(tr("Удалить безвозвратно?"))
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(if i18n::english() {
                    format!("Items to delete permanently: {}", paths.len())
                } else {
                    format!("Будет безвозвратно удалено объектов: {}", paths.len())
                });
                if let Some(first) = paths.first() {
                    ui.label(
                        egui::RichText::new(first.display().to_string())
                            .size(10.5)
                            .color(TEXT_MUTED),
                    );
                }
                ui.label(tr("Восстановить их будет нельзя."));
                ui.horizontal(|ui| {
                    if ui.button(tr("Удалить")).clicked() {
                        confirm = true;
                    }
                    if ui.button(tr("Отмена")).clicked()
                        || ui.input(|input| input.key_pressed(egui::Key::Escape))
                    {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.pending_permanent_delete = None;
            self.start_op(OpKind::Delete, paths, self.current_dir.clone());
        } else if cancel {
            self.pending_permanent_delete = None;
        }
    }

    fn copy_selection(&mut self, cut: bool) {
        if self.selection.is_empty() {
            self.status = tr("Ничего не выбрано").to_owned();
            return;
        }
        let paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
        let count = paths.len();
        let _ = shell_clipboard::set_files(&paths, cut);
        self.clipboard = Some(Clipboard {
            paths,
            mode: if cut { ClipMode::Move } else { ClipMode::Copy },
        });
        self.status = if i18n::english() {
            if cut {
                format!("Cut: {count}")
            } else {
                format!("Copied: {count}")
            }
        } else if cut {
            format!("Вырезано: {count}")
        } else {
            format!("Скопировано: {count}")
        };
    }

    fn paste(&mut self) {
        if let Some((paths, cut)) = shell_clipboard::get_files() {
            let kind = if cut { OpKind::Move } else { OpKind::Copy };
            if cut {
                self.clipboard = None;
                let _ = shell_clipboard::clear();
            }
            let destination = self.current_dir.clone();
            self.start_op(kind, paths, destination);
            return;
        }
        let Some(clipboard) = self.clipboard.clone() else {
            self.status = tr("Буфер обмена пуст").to_owned();
            return;
        };
        let kind = match clipboard.mode {
            ClipMode::Copy => OpKind::Copy,
            ClipMode::Move => OpKind::Move,
        };
        if matches!(kind, OpKind::Move) {
            self.clipboard = None;
        }
        let destination = self.current_dir.clone();
        self.start_op(kind, clipboard.paths, destination);
    }

    fn delete_selection(&mut self, permanent: bool) {
        // На случай, если строка выделена, но набор выделения пуст.
        if self.selection.is_empty() {
            if let Some(path) = self.selected.clone() {
                self.selection.insert(path);
            }
        }
        if self.selection.is_empty() {
            self.status = tr("Ничего не выбрано").to_owned();
            return;
        }
        let sources: Vec<PathBuf> = self.selection.iter().cloned().collect();
        if sources.iter().any(|path| path.parent().is_none()) {
            self.status = tr("Корень диска удалить нельзя").to_owned();
            return;
        }
        if permanent && self.confirm_permanent_delete {
            self.pending_permanent_delete = Some(sources);
            return;
        }
        let (kind, destination) = if permanent {
            (OpKind::Delete, self.current_dir.clone())
        } else {
            (OpKind::Trash, fs_ops::new_trash_batch_dir())
        };
        self.start_op(kind, sources, destination);
    }

    fn purge_search_results(&mut self, removed: &[PathBuf]) {
        if self.search_results.is_empty() {
            return;
        }
        let before = self.search_results.len();
        self.search_results
            .retain(|hit| !removed.iter().any(|path| hit.path.starts_with(path)));
        self.selection
            .retain(|path| !removed.iter().any(|removed| path.starts_with(removed)));
        if self.search_results.len() != before {
            self.selected = None;
            self.anchor = None;
            self.search_summary = tr_msg(
            format!("Готово: найдено {}", self.search_results.len()),
            format!("Done: found {}", self.search_results.len()),
        );
        }
    }

    fn select_all(&mut self) {
        if self.search_state.is_some()
            || !self.search_results.is_empty()
            || !self.search_summary.is_empty()
        {
            self.selection = self
                .search_results
                .iter()
                .map(|hit| hit.path.clone())
                .collect();
            self.anchor = self.search_results.last().map(|hit| hit.path.clone());
            self.selected = self.search_results.last().map(|hit| hit.path.clone());
        } else {
            let query = SearchQuery::new(
                &self.filter_text,
                SearchMode::from_index(self.search_mode_index),
            );
            let visible: Vec<&FsEntry> = self
                .entries
                .iter()
                .filter(|entry| filter_matches(&query, entry))
                .collect();
            self.selection = visible.iter().map(|entry| entry.path.clone()).collect();
            self.anchor = visible.last().map(|entry| entry.path.clone());
            self.selected = visible.last().map(|entry| entry.path.clone());
        }
        self.status = if i18n::english() {
            format!("Selected: {}", self.selection.len())
        } else {
            format!("Выбрано: {}", self.selection.len())
        };
    }

    fn move_selection(&mut self, delta: isize, extend: bool) {
        let search_active = self.search_state.is_some()
            || !self.search_results.is_empty()
            || !self.search_summary.is_empty();
        let paths: Vec<PathBuf> = if search_active {
            self.search_results
                .iter()
                .map(|hit| hit.path.clone())
                .collect()
        } else {
            self.filter_indices
                .iter()
                .filter_map(|&index| self.entries.get(index).map(|entry| entry.path.clone()))
                .collect()
        };
        if paths.is_empty() {
            return;
        }

        let current = self
            .selected
            .as_ref()
            .and_then(|path| paths.iter().position(|candidate| candidate == path));
        let position = current.unwrap_or(if delta < 0 { paths.len() - 1 } else { 0 });
        let next = if delta < 0 {
            position.saturating_sub(1)
        } else {
            (position + 1).min(paths.len() - 1)
        };

        if extend {
            let anchor_position = self
                .anchor
                .as_ref()
                .and_then(|path| paths.iter().position(|candidate| candidate == path))
                .unwrap_or(position);
            let (start, end) = if anchor_position <= next {
                (anchor_position, next)
            } else {
                (next, anchor_position)
            };
            self.selection = paths[start..=end].iter().cloned().collect();
        } else {
            self.selection.clear();
            self.selection.insert(paths[next].clone());
            self.anchor = Some(paths[next].clone());
        }
        self.selected = Some(paths[next].clone());
        self.scroll_to_row = Some(next);
    }

    fn copy_paths_to_clipboard(&mut self, ctx: &egui::Context) {
        if self.selection.is_empty() {
            self.status = tr("Ничего не выбрано").to_owned();
            return;
        }
        let count = self.selection.len();
        let text = self
            .selection
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\r\n");
        ctx.copy_text(text);
        self.status = tr_msg(
            format!("Скопировано путей: {count}"),
            format!("Paths copied: {count}"),
        );
    }

    fn create_folder(&mut self) {
        let dir = self.current_dir.clone();
        self.create_in(CreateKind::Folder, &dir);
    }

    /// Создаёт папку или файл в указанной папке. Если это текущая папка,
    /// сразу выделяет объект и открывает переименование, как в Проводнике.
    fn create_in(&mut self, kind: CreateKind, dir: &Path) {
        if !path_is_dir(dir) {
            self.status = tr_msg(
                format!("Папка не найдена: {}", dir.display()),
                format!("Folder not found: {}", dir.display()),
            );
            return;
        }
        let same_dir = dir == self.current_dir;
        if same_dir {
            if self.search_state.is_some() || !self.search_results.is_empty() {
                self.clear_search();
            }
            if !self.filter_text.is_empty() {
                self.clear_filter();
            }
        }

        let (name, result) = match kind {
            CreateKind::Folder => {
                let name = unique_child_name(dir, tr("Новая папка"), None);
                let result = fs::create_dir(crate::path_util::extended(&dir.join(&name)));
                (name, result)
            }
            CreateKind::Text => {
                let name = unique_child_name(dir, tr("Новый текстовый документ"), Some("txt"));
                let result = fs::write(crate::path_util::extended(&dir.join(&name)), b"");
                (name, result)
            }
            CreateKind::Word => {
                let name = unique_child_name(dir, tr("Новый документ Word"), Some("docx"));
                let result = write_docx(&dir.join(&name));
                (name, result)
            }
            CreateKind::Excel => {
                let name = unique_child_name(dir, tr("Новая таблица Excel"), Some("xlsx"));
                let result = write_xlsx(&dir.join(&name));
                (name, result)
            }
            CreateKind::Zip => {
                let name = unique_child_name(dir, tr("Новый архив"), Some("zip"));
                let result = write_empty_zip(&dir.join(&name));
                (name, result)
            }
            CreateKind::EmptyFile => {
                let name = unique_child_name(dir, tr("Новый файл"), None);
                let result = fs::write(crate::path_util::extended(&dir.join(&name)), b"");
                (name, result)
            }
        };

        match result {
            Ok(()) => {
                let path = dir.join(&name);
                let status = if i18n::english() {
                    match kind {
                        CreateKind::Folder => format!("Folder created: {name}"),
                        _ => format!("File created: {name}"),
                    }
                } else {
                    match kind {
                        CreateKind::Folder => format!("Создана папка: {name}"),
                        _ => format!("Создан файл: {name}"),
                    }
                };
                self.invalidate_index();
                self.background_menu = None;
                if same_dir {
                    self.refresh_directory();
                    self.selection.clear();
                    self.selection.insert(path.clone());
                    self.selected = Some(path.clone());
                    self.anchor = Some(path.clone());
                    self.rename = Some(RenameDialog {
                        path,
                        name,
                        focus: true,
                    });
                }
                self.status = status;
            }
            Err(error) => self.status = tr_msg(
                format!("Не удалось создать: {error}"),
                format!("Could not create: {error}"),
            ),
        }
    }

    /// Ярлык Windows (.lnk) на выбранный файл или папку.
    fn create_shortcut(&mut self, target: &Path) {
        let parent = target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.current_dir.clone());
        let base = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr("Ярлык").to_owned());
        let shortcut_name = if i18n::english() {
            format!("{base} — shortcut")
        } else {
            format!("{base} — ярлык")
        };
        let name = unique_child_name(&parent, &shortcut_name, Some("lnk"));
        let path = parent.join(&name);
        match shell_shortcut(target, &path) {
            Ok(()) => {
                self.invalidate_index();
                if parent == self.current_dir {
                    self.refresh_directory();
                    self.selection.clear();
                    self.selection.insert(path.clone());
                    self.selected = Some(path.clone());
                    self.anchor = Some(path);
                }
                self.status = if i18n::english() {
            format!("Shortcut created: {name}")
        } else {
            format!("Создан ярлык: {name}")
        };
            }
            Err(error) => self.status = tr_msg(
                format!("Не удалось создать ярлык: {error}"),
                format!("Could not create shortcut: {error}"),
            ),
        }
    }

    /// Правый клик по пустому месту открывает меню «Создать», как в Проводнике.
    fn arm_background_menu(&mut self, ui: &egui::Ui) {
        let position = ui.input(|input| {
            input
                .pointer
                .secondary_clicked()
                .then(|| input.pointer.interact_pos())
                .flatten()
        });
        if let Some(position) = position {
            if ui.max_rect().contains(position) {
                self.background_menu = Some((position, Instant::now()));
            }
        }
    }

    fn draw_background_menu(&mut self, ctx: &egui::Context) {
        let Some((position, opened)) = self.background_menu else {
            return;
        };
        let just_opened = opened.elapsed() < Duration::from_millis(120);
        let screen = ctx.screen_rect();
        let position = egui::pos2(
            position.x.min(screen.right() - 250.0).max(screen.left() + 4.0),
            position.y.min(screen.bottom() - 240.0).max(screen.top() + 4.0),
        );
        let mut create = None;
        let mut paste = false;
        let mut refresh = false;
        let mut sort_changed = false;
        let mut close = false;
        let mut sort = self.sort;
        let dir = self.current_dir.clone();

        let area = egui::Area::new(egui::Id::new("korobka_background_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(225.0);
                    create_submenu(ui, &dir, &mut create);
                    ui.separator();
                    if ui.button(tr("Вставить            Ctrl+V")).clicked() {
                        paste = true;
                        close = true;
                    }
                    if ui.button(tr("Обновить             F5")).clicked() {
                        refresh = true;
                        close = true;
                    }
                    ui.separator();
                    ui.menu_button(tr("Сортировка"), |ui| {
                        sort_changed = sort_menu(ui, &mut sort);
                    });
                });
            });

        let clicked_outside = ctx.input(|input| {
            input.pointer.any_click()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|position| !area.response.rect.contains(position))
        });
        if ctx.input(|input| input.key_pressed(egui::Key::Escape))
            || (!just_opened && clicked_outside)
            || create.is_some()
        {
            close = true;
        }
        if close {
            self.background_menu = None;
        }
        if sort_changed {
            self.sort = sort;
            self.sort_entries();
            self.rebuild_filter();
            self.background_menu = None;
        }
        if let Some((kind, target)) = create {
            self.create_in(kind, &target);
        }
        if paste {
            self.paste();
        }
        if refresh {
            self.nav_refresh();
        }
    }

    fn open_selected(&mut self) {
        if self.selection.len() != 1 {
            return;
        }
        let Some(path) = self.selection.iter().next().cloned() else {
            return;
        };
        let is_dir = path_is_dir(&path);
        self.open_path_item(path, is_dir);
    }

    fn start_op(&mut self, kind: OpKind, sources: Vec<PathBuf>, destination: PathBuf) {
        if self.op_state.is_some() {
            self.status = tr("Дождитесь завершения текущей операции").to_owned();
            return;
        }
        match fs_ops::start(kind, sources, destination) {
            Ok(state) => {
                self.op_state = Some(state);
                self.op_conflict = None;
                self.op_apply_all = false;
                self.op_progress = format!("{}:", kind.label());
            }
            Err(error) => self.status = error,
        }
    }

    fn poll_ops(&mut self, ctx: &egui::Context) {
        let Some(state) = self.op_state.as_ref() else {
            return;
        };
        let decisions = state.decisions.clone();
        let mut events = Vec::new();
        let mut disconnected = false;

        loop {
            match state.receiver.try_recv() {
                Ok(message) => {
                    events.push(message);
                    if events.len() >= 128 {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        let mut terminal = disconnected;
        for message in events {
            match message {
                OpMessage::Progress {
                    done,
                    total,
                    current,
                } => {
                    let name = current
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.op_progress = format!("{name} ({done}/{total})");
                }
                OpMessage::Conflict { source, target } => {
                    self.op_conflict = Some(OpConflict {
                        source,
                        target,
                        sender: decisions.clone(),
                    });
                }
                OpMessage::Finished {
                    kind,
                    completed,
                    skipped,
                    replaced,
                    elapsed,
                } => {
                    terminal = true;
                    let changed_paths: Vec<PathBuf> = match kind {
                        OpKind::Move | OpKind::Trash | OpKind::Delete => completed
                            .iter()
                            .map(|(source, _)| source.clone())
                            .collect(),
                        OpKind::Copy => Vec::new(),
                    };
                    if !replaced {
                        match kind {
                            OpKind::Copy => {
                                let created: Vec<PathBuf> =
                                    completed.iter().map(|(_, target)| target.clone()).collect();
                                if !created.is_empty() {
                                    self.push_undo(UndoAction::RemoveCreated(created));
                                }
                            }
                            OpKind::Move | OpKind::Trash => {
                                let pairs: Vec<(PathBuf, PathBuf)> = completed
                                    .iter()
                                    .map(|(source, target)| (target.clone(), source.clone()))
                                    .collect();
                                if !pairs.is_empty() {
                                    self.push_undo(UndoAction::MoveBack(pairs));
                                }
                            }
                            OpKind::Delete => {}
                        }
                    }
                    let failed_count = skipped.len();
                    let errors = skipped
                        .first()
                        .map(|(_, error)| error.clone())
                        .unwrap_or_default();
                    self.status = if i18n::english() {
                        format!(
                            "{}: {} items in {:.1} s{}",
                            kind.label(),
                            completed.len(),
                            elapsed.as_secs_f64(),
                            if skipped.is_empty() {
                                String::new()
                            } else {
                                format!(". Errors ({failed_count}): {errors}")
                            }
                        )
                    } else {
                        format!(
                            "{}: {} объектов за {:.1} с{}",
                            kind.label(),
                            completed.len(),
                            elapsed.as_secs_f64(),
                            if skipped.is_empty() {
                                String::new()
                            } else {
                                format!(". Ошибки ({failed_count}): {errors}")
                            }
                        )
                    };
                    self.op_progress.clear();
                    if kind == OpKind::Trash && self.trash_open {
                        self.load_trash();
                    }
                    // Если открыта выдача поиска, удалённые строки должны
                    // исчезнуть сразу, иначе кажется, что кнопка не сработала.
                    if !changed_paths.is_empty() {
                        self.purge_search_results(&changed_paths);
                    }
                    if matches!(kind, OpKind::Delete | OpKind::Trash) && !skipped.is_empty() {
                        let paths = skipped
                            .into_iter()
                            .map(|(path, _)| path)
                            .collect::<Vec<_>>();
                        self.start_lock_check(kind, paths);
                        ctx.request_repaint_after(Duration::from_millis(50));
                    }
                }
                OpMessage::Failed(error) => {
                    terminal = true;
                    self.status = tr_msg(
            format!("Ошибка операции: {error}"),
            format!("Operation error: {error}"),
        );
                    self.op_progress.clear();
                }
            }
        }

        if terminal {
            self.op_conflict = None;
            self.op_apply_all = false;
            self.op_state = None;
            self.invalidate_index();
            self.load_directory();
        } else if self.op_state.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn draw_op_conflict(&mut self, ctx: &egui::Context) {
        let Some(conflict) = self.op_conflict.as_ref() else {
            return;
        };
        let source = conflict.source.clone();
        let target = conflict.target.clone();
        let sender = conflict.sender.clone();
        let mut apply_all = self.op_apply_all;
        let mut open = true;
        let mut choice = None;
        let escape = ctx.input(|input| input.key_pressed(egui::Key::Escape));

        egui::Window::new(tr("Конфликт"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(tr("Объект уже существует:"))
                        .size(13.0)
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(target.display().to_string())
                        .size(11.0)
                        .color(TEXT_MUTED),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if i18n::english() {
                        format!("Source: {}", source.display())
                    } else {
                        format!("Источник: {}", source.display())
                    })
                    .size(10.5)
                    .color(TEXT_FAINT),
                );
                ui.add_space(8.0);
                ui.checkbox(&mut apply_all, tr("Применить ко всем"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Заменить")).clicked() {
                        choice = Some(ConflictChoice::Replace);
                    }
                    if ui.button(tr("Оставить оба")).clicked() {
                        choice = Some(ConflictChoice::KeepBoth);
                    }
                    if ui.button(tr("Пропустить")).clicked() {
                        choice = Some(ConflictChoice::Skip);
                    }
                    if ui.button(tr("Отмена")).clicked() {
                        choice = Some(ConflictChoice::Cancel);
                    }
                });
            });

        if let Some(choice) = choice {
            let _ = sender.send(ConflictDecision {
                choice,
                apply_to_all: apply_all,
            });
            self.op_conflict = None;
            self.op_apply_all = false;
        } else if !open || escape {
            let _ = sender.send(ConflictDecision {
                choice: ConflictChoice::Cancel,
                apply_to_all: false,
            });
            self.op_conflict = None;
            self.op_apply_all = false;
        } else {
            self.op_apply_all = apply_all;
        }
    }

    fn start_lock_check(&mut self, kind: OpKind, paths: Vec<PathBuf>) {
        if paths.is_empty() || self.lock_flow.is_some() || self.lock_dialog.is_some() {
            return;
        }
        self.status = tr("Проверяю, что удерживает файл…").to_owned();
        let (tx, rx) = mpsc::channel();
        let check_paths = paths.clone();
        let spawned = thread::Builder::new()
            .name("korobka-locks".to_owned())
            .spawn(move || {
                let scan = locks::find(&check_paths);
                let _ = tx.send(LockMessage::Checked { kind, paths, scan });
            });
        match spawned {
            Ok(_) => self.lock_flow = Some(LockFlow { receiver: rx }),
            Err(error) => self.status = tr_msg(
                format!("Не удалось проверить занятость: {error}"),
                format!("Could not check locks: {error}"),
            ),
        }
    }

    fn poll_locks(&mut self, ctx: &egui::Context) {
        let Some(flow) = self.lock_flow.take() else {
            return;
        };
        match flow.receiver.try_recv() {
            Ok(LockMessage::Checked { kind, paths, scan }) => match scan {
                Ok(scan) if !scan.processes.is_empty() => {
                    let selected = scan
                        .processes
                        .iter()
                        .filter(|process| process.selectable())
                        .map(|process| process.pid)
                        .collect();
                    self.lock_dialog = Some(LockDialog {
                        kind,
                        paths,
                        blocked_paths: scan.blocked_paths,
                        processes: scan.processes,
                        selected,
                        busy: false,
                        error: String::new(),
                    });
                }
                Ok(_) => {
                    self.status = tr_msg(
                        "Занятых процессов не найдено — можно повторить удаление".to_owned(),
                        "No locking processes found — you can retry deletion".to_owned(),
                    );
                }
                Err(error) => {
                    self.status = tr_msg(
            format!("Не удалось определить занявшие файл процессы: {error}"),
            format!("Could not identify processes locking the file: {error}"),
        );
                }
            },
            Ok(LockMessage::Closed {
                kind,
                paths,
                result,
            }) => match result {
                Ok(()) => {
                    self.lock_dialog = None;
                    self.retry_delete(kind, paths);
                    ctx.request_repaint();
                }
                Err(error) => {
                    if let Some(dialog) = self.lock_dialog.as_mut() {
                        dialog.busy = false;
                        dialog.error = error;
                    } else {
                        self.status = error;
                    }
                }
            },
            Err(TryRecvError::Empty) => {
                self.lock_flow = Some(flow);
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(TryRecvError::Disconnected) => {
                self.status = tr("Проверка занятости прервана").to_owned();
            }
        }
    }

    fn retry_delete(&mut self, kind: OpKind, paths: Vec<PathBuf>) {
        let destination = if kind == OpKind::Trash {
            fs_ops::new_trash_batch_dir()
        } else {
            self.current_dir.clone()
        };
        self.start_op(kind, paths, destination);
    }

    fn confirm_close_and_delete(&mut self) {
        let Some(dialog) = self.lock_dialog.as_mut() else {
            return;
        };
        if dialog.busy || dialog.selected.is_empty() {
            return;
        }
        dialog.busy = true;
        dialog.error.clear();
        let kind = dialog.kind;
        let paths = dialog.paths.clone();
        let selected = dialog.selected.iter().copied().collect::<Vec<_>>();
        let (tx, rx) = mpsc::channel();
        let close_paths = paths.clone();
        let spawned = thread::Builder::new()
            .name("korobka-unlock".to_owned())
            .spawn(move || {
                let result = locks::shutdown(&close_paths, &selected);
                if result.is_ok() {
                    thread::sleep(Duration::from_millis(250));
                }
                let _ = tx.send(LockMessage::Closed {
                    kind,
                    paths,
                    result,
                });
            });
        match spawned {
            Ok(_) => self.lock_flow = Some(LockFlow { receiver: rx }),
            Err(error) => {
                if let Some(dialog) = self.lock_dialog.as_mut() {
                    dialog.busy = false;
                    dialog.error = tr_msg(
            format!("Не удалось запустить закрытие процессов: {error}"),
            format!("Could not start closing processes: {error}"),
        );
                }
            }
        }
    }

    fn refresh_lock_check(&mut self) {
        let Some(dialog) = self.lock_dialog.as_ref() else {
            return;
        };
        let kind = dialog.kind;
        let paths = dialog.paths.clone();
        self.lock_dialog = None;
        self.status = tr("Проверяю занятость…").to_owned();
        self.start_lock_check(kind, paths);
    }

    fn draw_lock_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.lock_dialog.as_ref() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        let mut refresh = false;
        let busy = dialog.busy;
        let confirm_key = ctx.input(|input| input.key_pressed(egui::Key::Enter));
        let cancel_key = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let mut selected = dialog.selected.clone();
        let target_paths = if dialog.blocked_paths.is_empty() {
            &dialog.paths
        } else {
            &dialog.blocked_paths
        };
        let title = match dialog.kind {
            OpKind::Trash => tr("Удаление в корзину"),
            OpKind::Delete => tr("Удаление"),
            _ => tr("Файл занят"),
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(560.0);
                let name = target_paths
                    .first()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| tr("Файл").to_owned());
                ui.label(egui::RichText::new(name).strong().size(15.0));
                if target_paths.len() > 1 {
                    ui.label(
                        egui::RichText::new(if i18n::english() {
                            format!("and {} more items", target_paths.len() - 1)
                        } else {
                            format!("и ещё {} объектов", target_paths.len() - 1)
                        })
                        .weak(),
                    );
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(if dialog.blocked_paths.is_empty() {
                        tr("Выбрано:")
                    } else {
                        tr("Блокируют удаление:")
                    })
                    .weak(),
                );
                for path in target_paths.iter().take(3) {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string())
                                .monospace()
                                .small(),
                        )
                        .selectable(true),
                    );
                }
                if target_paths.len() > 3 {
                    ui.label(
                        egui::RichText::new(if i18n::english() {
                            format!("and {} more paths", target_paths.len() - 3)
                        } else {
                            format!("и ещё {} путей", target_paths.len() - 3)
                        })
                        .weak(),
                    );
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new(tr("Процессы:")).weak());
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for process in &dialog.processes {
                            let selectable = process.selectable();
                            let mut checked = selected.contains(&process.pid);
                            ui.horizontal(|ui| {
                                let response = ui.add_enabled(
                                    selectable,
                                    egui::Checkbox::without_text(&mut checked),
                                );
                                if selectable && response.changed() {
                                    if checked {
                                        selected.insert(process.pid);
                                    } else {
                                        selected.remove(&process.pid);
                                    }
                                }
                                ui.label(egui::RichText::new(&process.name).strong());
                                if !process.exe.is_empty()
                                    && !process.exe.eq_ignore_ascii_case(&process.name)
                                {
                                    ui.label(egui::RichText::new(&process.exe).small().weak());
                                }
                                ui.label(
                                    egui::RichText::new(format!("PID {}", process.pid)).weak(),
                                );
                                if process.shell {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(226, 170, 110),
                                        tr("Проводник будет перезапущен"),
                                    );
                                } else if !process.service.is_empty() {
                                    ui.label(
                                        egui::RichText::new(if i18n::english() {
                                            format!("service {}", process.service)
                                        } else {
                                            format!("служба {}", process.service)
                                        })
                                            .small()
                                            .weak(),
                                    );
                                } else if process.critical {
                                    ui.label(egui::RichText::new(tr("системный")).small().weak());
                                } else if process.protected {
                                    ui.label(egui::RichText::new(tr("защищён")).small().weak());
                                }
                            });
                        }
                    });
                if !dialog.error.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(224, 120, 120), &dialog.error);
                }
                ui.add_space(6.0);
                ui.label(tr("Закрыть выбранные процессы? Несохранённые данные могут быть потеряны."));
                ui.horizontal(|ui| {
                    let can_close = !busy && !selected.is_empty();
                    let response =
                        ui.add_enabled(can_close, egui::Button::new(tr("Закрыть выбранные и удалить")));
                    if response.clicked() {
                        confirm = true;
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Обновить список")))
                        .clicked()
                    {
                        refresh = true;
                    }
                    if ui.add_enabled(!busy, egui::Button::new(tr("Отмена"))).clicked() {
                        cancel = true;
                    }
                    if busy {
                        ui.spinner();
                        ui.label(egui::RichText::new(tr("Закрываю процессы…")).weak());
                    }
                });
            });

        if selected != dialog.selected {
            if let Some(dialog) = self.lock_dialog.as_mut() {
                dialog.selected = selected;
            }
        }
        let can_confirm = self
            .lock_dialog
            .as_ref()
            .is_some_and(|dialog| !dialog.busy && !dialog.selected.is_empty());
        if confirm || (confirm_key && can_confirm) {
            self.confirm_close_and_delete();
        } else if refresh {
            self.refresh_lock_check();
        } else if cancel || (cancel_key && !busy) || (!open && !busy) {
            self.lock_dialog = None;
        }
    }

    fn push_undo(&mut self, action: UndoAction) {
        self.undo_stack.push(action);
        if self.undo_stack.len() > MAX_UNDO_STEPS {
            self.undo_stack.remove(0);
        }
    }

    fn invalidate_index(&mut self) {
        self.dir_cache.clear();
        // Не удаляем кэши (их может быть много и они большие), а помечаем их
        // устаревшими: следующий поиск перестроит нужный индекс.
        search::touch_generation();
    }

    fn undo(&mut self) {
        let Some(action) = self.undo_stack.pop() else {
            self.status = tr("Нечего отменять").to_owned();
            return;
        };
        let mut errors = Vec::new();
        match action {
            UndoAction::MoveBack(pairs) => {
                for (from, to) in pairs {
                    if let Err(err) = fs_ops::move_item(&from, &to) {
                        errors.push(format!("{}: {err}", from.display()));
                    }
                }
            }
            UndoAction::RemoveCreated(paths) => {
                for path in paths {
                    if let Err(err) = fs_ops::remove_any(&path) {
                        errors.push(format!("{}: {err}", path.display()));
                    }
                }
            }
        }
        self.dir_cache.clear();
        self.load_directory();
        self.status = if errors.is_empty() {
            if i18n::english() {
                "Cancelled".to_owned()
            } else {
                "Отменено".to_owned()
            }
        } else if i18n::english() {
            format!("Cancel completed with errors: {}", errors.join("; "))
        } else {
            format!("Отмена с ошибками: {}", errors.join("; "))
        };
    }

    fn poll_dir_load(&mut self, ctx: &egui::Context) {
        let Some(load) = self.dir_load.take() else {
            return;
        };
        match load.receiver.try_recv() {
            Ok(result) => {
                if result.path != self.current_dir {
                    return;
                }
                self.entries = result.entries;
                if self.sort != result.sort {
                    self.sort_entries();
                }
                self.filter_dirty = true;
                self.rebuild_filter();
                self.status = match result.error {
                    Some(error) => {
                        if i18n::english() {
                            format!("Could not read folder: {error}")
                        } else {
                            format!("Не удалось прочитать папку: {error}")
                        }
                    }
                    None => {
                        if i18n::english() {
                            format!("Done · {} ms", result.elapsed.as_millis())
                        } else {
                            format!("Готово · {} мс", result.elapsed.as_millis())
                        }
                    }
                };
            }
            Err(TryRecvError::Empty) => {
                self.dir_load = Some(load);
                ctx.request_repaint_after(Duration::from_millis(30));
            }
            Err(TryRecvError::Disconnected) => {
                self.status = tr("Чтение папки прервано").to_owned();
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        if self.rename.is_some() || self.lock_dialog.is_some() {
            return;
        }
        let (paths, move_files) = ctx.input(|input| {
            let paths = input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect::<Vec<_>>();
            (paths, input.modifiers.shift)
        });
        if paths.is_empty() {
            return;
        }
        let kind = if move_files {
            OpKind::Move
        } else {
            OpKind::Copy
        };
        self.start_op(kind, paths, self.current_dir.clone());
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input()
            || self.rename.is_some()
            || self.lock_dialog.is_some()
            || self.settings_open
            || self.picker_open
            || self.op_conflict.is_some()
            || self.background_menu.is_some()
            || self.pending_permanent_delete.is_some()
            || self.trash_open
            || self.trash_delete_pending.is_some()
        {
            return;
        }
        let ctrl = ctx.input(|input| input.modifiers.command);
        let shift = ctx.input(|input| input.modifiers.shift);
        let alt = ctx.input(|input| input.modifiers.alt);
        let pressed = |key: egui::Key| ctx.input(|input| input.key_pressed(key));

        if self.archive.is_some() {
            if alt && pressed(egui::Key::ArrowLeft) {
                self.archive_back();
            } else if alt && pressed(egui::Key::ArrowRight) {
                self.archive_forward();
            } else if pressed(egui::Key::F5) {
                self.archive_reload();
            } else if pressed(egui::Key::Backspace) {
                self.archive_up();
            } else if pressed(egui::Key::Enter) {
                self.archive_open_selected();
            } else if pressed(egui::Key::Escape) {
                self.close_archive();
                release_text_focus(ctx);
            }
            return;
        }

        if ctrl && pressed(egui::Key::T) {
            let path = self.current_dir.clone();
            self.new_tab(path);
        } else if ctrl && pressed(egui::Key::W) {
            self.close_tab(self.active_tab);
        } else if ctrl && pressed(egui::Key::Tab) {
            if self.tabs.len() > 1 {
                let count = self.tabs.len();
                let index = if shift {
                    (self.active_tab + count - 1) % count
                } else {
                    (self.active_tab + 1) % count
                };
                self.switch_tab(index);
            }
        } else if ctrl && pressed(egui::Key::L) {
            self.address_editing = true;
            self.focus_address = true;
            self.path_text = self.current_dir.display().to_string();
        } else if ctrl && pressed(egui::Key::F) {
            self.focus_search = true;
        } else if ctrl && pressed(egui::Key::Comma) {
            self.settings_open = true;
        } else if alt && pressed(egui::Key::ArrowLeft) {
            self.nav_back();
        } else if alt && pressed(egui::Key::ArrowRight) {
            self.nav_forward();
        } else if ctrl && shift && pressed(egui::Key::N) {
            self.create_folder();
        } else if pressed(egui::Key::ArrowDown) {
            self.move_selection(1, shift);
        } else if pressed(egui::Key::ArrowUp) {
            self.move_selection(-1, shift);
        } else if pressed(egui::Key::F2) {
            self.begin_rename();
        } else if pressed(egui::Key::F5) {
            self.nav_refresh();
        } else if ctrl && pressed(egui::Key::A) {
            self.select_all();
        } else if pressed(egui::Key::Delete) {
            self.delete_selection(shift);
        } else if ctrl && pressed(egui::Key::C) {
            self.copy_selection(false);
        } else if ctrl && pressed(egui::Key::X) {
            self.copy_selection(true);
        } else if ctrl && pressed(egui::Key::V) {
            self.paste();
        } else if ctrl && pressed(egui::Key::Z) {
            self.undo();
        } else if pressed(egui::Key::Escape) {
            if self.search_state.is_some()
                || !self.search_results.is_empty()
                || !self.search_summary.is_empty()
                || !self.filter_text.is_empty()
            {
                self.clear_search();
                self.rebuild_filter();
            }
        } else if pressed(egui::Key::Enter) {
            self.open_selected();
        } else if pressed(egui::Key::Backspace) {
            self.nav_up();
        }
    }

    fn draw_title_bar(&mut self, ctx: &egui::Context) {
        let mut minimize = false;
        let mut maximize = false;
        let mut close = false;
        let is_maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));

        egui::TopBottomPanel::top("title_bar")
            .exact_height(38.0)
            .frame(
                egui::Frame::NONE
                    .fill(glass_strong())
                    .inner_margin(egui::Margin::symmetric(10, 5)),
            )
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                let drag_rect = egui::Rect::from_min_max(
                    bar.left_top(),
                    egui::pos2((bar.right() - 132.0).max(bar.left()), bar.bottom()),
                );
                let drag = ui.interact(
                    drag_rect,
                    ui.id().with("title_drag"),
                    egui::Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    maximize = true;
                }

                ui.horizontal(|ui| {
                    let icon_rect = egui::Rect::from_center_size(
                        egui::pos2(ui.cursor().left() + 9.0, ui.cursor().center().y),
                        egui::vec2(18.0, 18.0),
                    );
                    ui.allocate_space(egui::vec2(18.0, 18.0));
                    paint_icon(ui.painter(), icon_rect, Icon::Folder, accent_bright());
                    ui.label(
                        egui::RichText::new("Korobka Explorer")
                            .size(12.5)
                            .strong()
                            .color(TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if window_control(ui, 2).on_hover_text(tr("Закрыть")).clicked() {
                            close = true;
                        }
                        if window_control(ui, 1)
                            .on_hover_text(if is_maximized {
                                tr("Восстановить")
                            } else {
                                tr("Развернуть")
                            })
                            .clicked()
                        {
                            maximize = true;
                        }
                        if window_control(ui, 0).on_hover_text(tr("Свернуть")).clicked() {
                            minimize = true;
                        }
                    });
                });
            });

        if minimize {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if maximize {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
        }
        if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn draw_tab_bar(&mut self, ctx: &egui::Context) {
        let mut switch_to = None;
        let mut close = None;
        let mut add = false;
        egui::TopBottomPanel::top("tab_bar")
            .exact_height(40.0)
            .frame(
                egui::Frame::NONE
                    .fill(glass_strong())
                    .stroke(egui::Stroke::new(1.0_f32, glass_border()))
                    .inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for index in 0..self.tabs.len() {
                        let active = index == self.active_tab;
                        let dir = if active {
                            &self.current_dir
                        } else {
                            &self.tabs[index].dir
                        };
                        let title = tab_title(dir);
                        let width = (title.chars().count() as f32 * 6.8 + 62.0).clamp(112.0, 220.0);
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::click());
                        if active {
                            ui.painter().rect_filled(rect, 8.0, glass_fill());
                            ui.painter().rect_stroke(
                                rect,
                                8.0,
                                egui::Stroke::new(1.0_f32, glass_border()),
                                egui::StrokeKind::Inside,
                            );
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(rect.left() + 8.0, rect.bottom() - 2.0),
                                    egui::vec2(rect.width() - 16.0, 2.0),
                                ),
                                1.0,
                                accent(),
                            );
                        } else if response.hovered() {
                            ui.painter().rect_filled(rect, 8.0, glass_soft());
                        }
                        let icon_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.left() + 17.0, rect.center().y),
                            egui::vec2(15.0, 15.0),
                        );
                        paint_icon(
                            ui.painter(),
                            icon_rect,
                            Icon::Folder,
                            if active { accent() } else { TEXT_FAINT },
                        );
                        let close_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.right() - 15.0, rect.center().y),
                            egui::vec2(20.0, 20.0),
                        );
                        let close_response = ui.interact(
                            close_rect,
                            ui.id().with(("tab_close", index)),
                            egui::Sense::click(),
                        );
                        let text_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.left() + 29.0, rect.top()),
                            egui::pos2(close_rect.left() - 3.0, rect.bottom()),
                        );
                        let painter = ui.painter().with_clip_rect(text_rect);
                        painter.text(
                            text_rect.left_center(),
                            egui::Align2::LEFT_CENTER,
                            title,
                            egui::FontId::proportional(12.0),
                            if active { TEXT } else { TEXT_MUTED },
                        );
                        if active || response.hovered() {
                            paint_icon(
                                ui.painter(),
                                close_rect.shrink(5.0),
                                Icon::Close,
                                if close_response.hovered() {
                                    TEXT
                                } else {
                                    TEXT_FAINT
                                },
                            );
                        }
                        if close_response.clicked() || response.middle_clicked() {
                            close = Some(index);
                        } else if response.clicked() {
                            switch_to = Some(index);
                        }
                    }
                    if icon_button(ui, Icon::Plus, tr("Новая вкладка (Ctrl+T)"), false, true).clicked()
                    {
                        add = true;
                    }
                });
                ui.painter().line_segment(
                    [ui.max_rect().left_bottom(), ui.max_rect().right_bottom()],
                    egui::Stroke::new(1.0_f32, BORDER_SOFT),
                );
            });
        if let Some(index) = close {
            self.close_tab(index);
        } else if let Some(index) = switch_to {
            self.switch_tab(index);
        } else if add {
            let path = self.current_dir.clone();
            self.new_tab(path);
        }
    }

    fn draw_nav_bar(&mut self, ctx: &egui::Context) {
        let mut go_back = false;
        let mut go_forward = false;
        let mut go_up = false;
        let mut refresh = false;
        let mut edit_address = false;
        let mut navigate: Option<PathBuf> = None;
        let mut search_request = false;
        let mut search_disk_request = false;
        let mut clear_filter = false;
        let mut mode_changed = false;
        egui::TopBottomPanel::top("nav_bar")
            .exact_height(48.0)
            .frame(
                egui::Frame::NONE
                    .fill(glass_fill())
                    .stroke(egui::Stroke::new(1.0_f32, glass_border()))
                    .inner_margin(egui::Margin::symmetric(8, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let can_back = self.archive.is_some()
                        || (self.history_pos > 0 && !self.history.is_empty());
                    let can_forward = self
                        .archive
                        .as_ref()
                        .is_some_and(|view| view.history_pos + 1 < view.history.len())
                        || (self.archive.is_none() && self.history_pos + 1 < self.history.len());
                    if icon_button(ui, Icon::Back, tr("Назад (Alt+←)"), false, can_back).clicked()
                    {
                        go_back = true;
                    }
                    if icon_button(ui, Icon::Forward, tr("Вперёд (Alt+→)"), false, can_forward)
                        .clicked()
                    {
                        go_forward = true;
                    }
                    if icon_button(ui, Icon::Up, tr("Вверх (Backspace)"), false, true).clicked() {
                        go_up = true;
                    }
                    if icon_button(ui, Icon::Refresh, tr("Обновить (F5)"), false, true).clicked()
                    {
                        refresh = true;
                    }
                    ui.add_space(4.0);
                    let search_width = (ui.available_width() * 0.3).clamp(260.0, 340.0);
                    let address_width = (ui.available_width() - search_width - 52.0).max(80.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(address_width, 30.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            if self.address_editing {
                                let response = ui.add_sized(
                                    [ui.available_width() - 32.0, 27.0],
                                    egui::TextEdit::singleline(&mut self.path_text)
                                        .hint_text(tr("Путь к папке")),
                                );
                                if self.focus_address {
                                    response.request_focus();
                                    self.focus_address = false;
                                }
                                if response.has_focus()
                                    && ui.input(|input| input.key_pressed(egui::Key::Escape))
                                {
                                    self.address_editing = false;
                                    self.path_text = self.current_dir.display().to_string();
                                    release_text_focus(ui.ctx());
                                }
                                if response.has_focus()
                                    && ui.input(|input| input.key_pressed(egui::Key::Enter))
                                {
                                    self.address_editing = false;
                                    navigate = Some(clean_path(&self.path_text));
                                }
                            } else {
                                let crumb_width = (ui.available_width() - 32.0).max(40.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(crumb_width, 28.0),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        navigate = breadcrumbs(ui, &self.current_dir);
                                    },
                                );
                                if icon_button(
                                    ui,
                                    Icon::Edit,
                                    tr("Изменить путь (Ctrl+L)"),
                                    false,
                                    true,
                                )
                                .clicked()
                                {
                                    edit_address = true;
                                }
                            }
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(
                            ui,
                            Icon::Close,
                            tr("Очистить поиск"),
                            false,
                            !self.filter_text.is_empty(),
                        )
                        .clicked()
                        {
                            clear_filter = true;
                        }
                        if icon_button(
                            ui,
                            Icon::Search,
                            tr("Искать на всём диске"),
                            false,
                            !self.filter_text.trim().is_empty(),
                        )
                        .clicked()
                        {
                            search_disk_request = true;
                        }
                        let exact = self.search_mode_index == 0;
                        let mode_title =
                            egui::RichText::new(if exact { tr("Точное") } else { tr("Частичное") })
                                .size(11.0);
                        ui.menu_button(mode_title, |ui| {
                            ui.set_min_width(150.0);
                            if ui
                                .selectable_label(exact, tr("Точное имя"))
                                .on_hover_text(tr("Имя файла должно совпасть целиком"))
                                .clicked()
                            {
                                if !exact {
                                    self.search_mode_index = 0;
                                    mode_changed = true;
                                }
                                ui.close_menu();
                            }
                            if ui
                                .selectable_label(!exact, tr("По части имени"))
                                .on_hover_text(tr("Достаточно вхождения в имя"))
                                .clicked()
                            {
                                if exact {
                                    self.search_mode_index = 1;
                                    mode_changed = true;
                                }
                                ui.close_menu();
                            }
                        })
                        .response
                        .on_hover_text(tr("Режим поиска: точное имя или вхождение"));
                        let response = ui
                            .add_sized(
                                [search_width - 140.0, 27.0],
                                egui::TextEdit::singleline(&mut self.filter_text)
                                    .id(egui::Id::new("korobka_search_field"))
                                    .hint_text(if exact {
                                        tr("Точное имя (Enter)…")
                                    } else {
                                        tr("Имя (Enter — поиск в папке)…")
                                    }),
                            )
                            .on_hover_text(tr("Enter — поиск в папке и подпапках, лупа — весь диск"));
                        if self.focus_search {
                            response.request_focus();
                            self.focus_search = false;
                        }
                        if response.changed() {
                            self.selection.clear();
                            self.selected = None;
                            self.anchor = None;
                            self.filter_dirty = true;
                            self.rebuild_filter();
                            // Ввод фильтрует текущую папку мгновенно. Диск не
                            // обходим: поиск запускается по Enter или кнопке.
                            if self.filter_text.trim().is_empty() {
                                self.clear_search();
                            } else {
                                self.reset_search_results();
                            }
                        }
                        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if enter && (response.has_focus() || response.lost_focus()) {
                            search_request = true;
                        }
                        if response.has_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Escape))
                        {
                            clear_filter = true;
                        }
                    });
                });
                ui.painter().line_segment(
                    [ui.max_rect().left_bottom(), ui.max_rect().right_bottom()],
                    egui::Stroke::new(1.0_f32, BORDER_SOFT),
                );
            });
        if go_back {
            self.nav_back();
        }
        if go_forward {
            self.nav_forward();
        }
        if go_up {
            self.nav_up();
        }
        if refresh {
            self.nav_refresh();
        }
        if edit_address {
            self.address_editing = true;
            self.focus_address = true;
            self.path_text = self.current_dir.display().to_string();
        }
        if let Some(path) = navigate {
            self.address_editing = false;
            release_text_focus(ctx);
            self.navigate_to(path);
        }
        if clear_filter {
            release_text_focus(ctx);
            self.clear_search();
            self.rebuild_filter();
        }
        if search_request || search_disk_request {
            // После Enter фокус должен уйти из поля, иначе Del продолжает
            // редактировать строку поиска вместо удаления выбранного файла.
            release_text_focus(ctx);
        }
        if search_request {
            self.start_search(SearchScope::Folder);
        }
        if search_disk_request {
            self.start_search(SearchScope::Volume);
        }
        if mode_changed {
            self.filter_dirty = true;
            self.rebuild_filter();
            // Если результаты уже показаны, перезапускаем тот же поиск
            // в новом режиме, иначе меняется только фильтр папки.
            let scope = self
                .search_state
                .as_ref()
                .map(|state| state.scope)
                .or(self.last_search_scope);
            if let Some(scope) = scope {
                if !self.filter_text.trim().is_empty()
                    && (self.search_state.is_some() || !self.search_results.is_empty())
                {
                    self.start_search(scope);
                }
            }
        }
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        let collapsed = self.sidebar_collapsed;
        let frame = egui::Frame::NONE
            .fill(glass_soft())
            .stroke(egui::Stroke::new(1.0_f32, glass_border()))
            .inner_margin(egui::Margin::symmetric(8, 8));
        let panel = egui::SidePanel::left("sidebar").frame(frame);
        let panel = if collapsed {
            panel.resizable(false).exact_width(48.0)
        } else {
            panel
                .resizable(true)
                .default_width(self.sidebar_width)
                .width_range(148.0..=240.0)
        };
        let mut toggle = false;
        let mut target: Option<PathBuf> = None;
        let mut open_trash = false;
        let response = panel.show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if collapsed {
                    if icon_button(ui, Icon::Sidebar, tr("Развернуть панель"), false, true).clicked()
                    {
                        toggle = true;
                    }
                } else {
                    ui.label(
                        egui::RichText::new(tr("БЫСТРЫЙ ДОСТУП"))
                            .size(10.0)
                            .color(TEXT_FAINT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Sidebar, tr("Свернуть панель"), false, true).clicked()
                        {
                            toggle = true;
                        }
                    });
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (label, path) in &self.places {
                        let selected = self.current_dir == *path;
                        if nav_item(ui, place_icon(path), label, selected, collapsed) {
                            target = Some(path.clone());
                        }
                    }
                    ui.add_space(4.0);
                    if nav_item(ui, Icon::Trash, tr("Корзина"), self.trash_open, collapsed) {
                        open_trash = true;
                    }
                    ui.add_space(8.0);
                    if !collapsed {
                        ui.label(egui::RichText::new(tr("ДИСКИ")).size(10.0).color(TEXT_FAINT));
                        ui.add_space(2.0);
                    }
                    for drive in &self.drives {
                        let selected = self.current_dir.starts_with(&drive.root);
                        let label = if drive.label.is_empty() {
                            format!("{}:", drive.letter)
                        } else {
                            format!("{}: {}", drive.letter, drive.label)
                        };
                        if nav_item(ui, Icon::Drive, &label, selected, collapsed) {
                            target = Some(drive.root.clone());
                        }
                    }
                });
        });
        if toggle {
            self.sidebar_collapsed = !self.sidebar_collapsed;
        }
        if !collapsed {
            self.sidebar_width = response.response.rect.width().clamp(148.0, 240.0);
        }
        if let Some(path) = target {
            self.navigate_to(path);
        }
        if open_trash {
            self.trash_open = true;
            self.load_trash();
        }
    }

    fn draw_properties_panel(&mut self, ctx: &egui::Context) {
        if !self.show_properties {
            return;
        }
        let selected_path = self
            .selected
            .clone()
            .or_else(|| self.selection.iter().next().cloned());
        let mut close = false;
        let mut native_properties = None;
        egui::SidePanel::right("properties")
            .resizable(true)
            .default_width(260.0)
            .width_range(220.0..=340.0)
            .frame(
                egui::Frame::NONE
                    .fill(glass_soft())
                    .stroke(egui::Stroke::new(1.0_f32, glass_border()))
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(tr("Свойства"))
                            .size(12.0)
                            .strong()
                            .color(TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, tr("Закрыть свойства"), false, true).clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(8.0);
                if self.selection.len() > 1 {
                    let total: u64 = self
                        .entries
                        .iter()
                        .filter(|entry| self.selection.contains(&entry.path))
                        .filter_map(|entry| entry.size)
                        .sum();
                    ui.label(
                        egui::RichText::new(selected_text(self.selection.len()))
                            .color(TEXT)
                            .size(14.0),
                    );
                    ui.label(
                        egui::RichText::new(if i18n::english() {
                            format!("Total size: {}", format_size(total))
                        } else {
                            format!("Суммарный размер: {}", format_size(total))
                        })
                            .color(TEXT_MUTED)
                            .size(12.0),
                    );
                } else if let Some(path) = selected_path {
                    let entry = self.entries.iter().find(|entry| entry.path == path);
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    let is_dir = entry
                        .map(|entry| entry.is_dir)
                        .unwrap_or_else(|| path_is_dir(&path));
                    ui.vertical_centered(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
                        paint_icon(
                            ui.painter(),
                            rect.shrink(8.0),
                            if is_dir { Icon::Folder } else { Icon::File },
                            if is_dir { WARNING } else { accent() },
                        );
                    });
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(name).size(14.0).strong().color(TEXT));
                    ui.add_space(4.0);
                    property_row(ui, tr("Тип"), entry.map(|entry| tr(entry.kind)).unwrap_or("—"));
                    if let Some(entry) = entry {
                        if !entry.is_dir {
                            property_row(ui, tr("Размер"), entry.size_text());
                        }
                        let modified_text = entry
                            .modified_text
                            .get_or_init(|| format_modified(entry.modified));
                        if !modified_text.is_empty() {
                            property_row(ui, tr("Изменён"), modified_text);
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(tr("Путь")).size(10.0).color(TEXT_FAINT));
                    ui.label(
                        egui::RichText::new(path.display().to_string())
                            .size(11.0)
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(8.0);
                    if ui.button(tr("Копировать путь")).clicked() {
                        ui.ctx().copy_text(path.display().to_string());
                    }
                    if ui.button(tr("Свойства Windows…")).clicked() {
                        native_properties = Some(path.clone());
                    }
                } else {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(tr("Ничего не выбрано"))
                                .size(12.0)
                                .color(TEXT_FAINT),
                        );
                    });
                }
            });
        if close {
            self.show_properties = false;
        }
        if let Some(path) = native_properties {
            if let Err(err) = show_shell_properties(&path) {
                self.status = tr_msg(
            format!("Не удалось открыть свойства: {err}"),
            format!("Could not open properties: {err}"),
        );
            }
        }
    }

    fn draw_archive(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let mut back = false;
        let mut up = false;
        let mut reload = false;
        let mut extract_all = false;
        let mut navigate = None;
        let mut open = None;
        let mut extract = None;
        let mut selected_request = None;
        let mut copy_path = None;

        let Some(view) = self.archive.as_mut() else {
            return;
        };

        ui.horizontal(|ui| {
            if icon_button(ui, Icon::Back, tr("Назад"), false, true).clicked() {
                back = true;
            }
            if icon_button(ui, Icon::Up, tr("Вверх"), false, true).clicked() {
                up = true;
            }
            ui.label(egui::RichText::new("ZIP").size(9.5).strong().color(WARNING));
            ui.label(
                egui::RichText::new(
                    view.zip_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| view.zip_path.display().to_string()),
                )
                .size(14.0)
                .strong()
                .color(TEXT),
            );
            ui.label(
                egui::RichText::new(items_text(view.entries.len()))
                    .size(10.5)
                    .color(TEXT_FAINT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Close, tr("Закрыть архив"), false, true).clicked()
                {
                    close = true;
                }
                if ui
                    .add_enabled(
                        !view.entries.is_empty(),
                        egui::Button::new(egui::RichText::new(tr("Извлечь всё")).size(11.0)),
                    )
                    .clicked()
                {
                    extract_all = true;
                }
                if icon_button(ui, Icon::Refresh, tr("Обновить архив"), false, true).clicked()
                {
                    reload = true;
                }
            });
        });

        ui.horizontal_wrapped(|ui| {
            let root_label = view
                .zip_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| view.zip_path.display().to_string());
            if view.inner.is_empty() {
                ui.label(
                    egui::RichText::new(&root_label)
                        .size(11.0)
                        .color(TEXT_MUTED),
                );
            } else {
                if ui
                    .link(egui::RichText::new(&root_label).size(11.0))
                    .clicked()
                {
                    navigate = Some(String::new());
                }
                let mut accumulated = String::new();
                for part in view.inner.trim_end_matches('/').split('/') {
                    if part.is_empty() {
                        continue;
                    }
                    accumulated.push_str(part);
                    accumulated.push('/');
                    ui.label(egui::RichText::new("/").size(11.0).color(TEXT_FAINT));
                    if accumulated == view.inner {
                        ui.label(egui::RichText::new(part).size(11.0).strong().color(TEXT));
                    } else if ui.link(egui::RichText::new(part).size(11.0)).clicked() {
                        navigate = Some(accumulated.clone());
                    }
                }
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if icon_button(
                ui,
                Icon::Close,
                tr("Очистить фильтр"),
                false,
                !view.filter.is_empty(),
            )
            .clicked()
            {
                view.filter.clear();
            }
            let width = (ui.available_width() - 4.0).max(80.0);
            ui.add_sized(
                [width, 24.0],
                egui::TextEdit::singleline(&mut view.filter)
                    .id(egui::Id::new("korobka_archive_filter"))
                    .hint_text(tr("Фильтр внутри архива…")),
            );
        });
        ui.add_space(2.0);

        if let Some(error) = &view.error {
            ui.add_space(30.0);
            empty_state(ui, Icon::File, tr("Не удалось открыть архив"), error);
        } else if view.entries.is_empty() {
            ui.add_space(30.0);
            empty_state(ui, Icon::Folder, tr("Архив пуст"), tr("В этом ZIP нет файлов"));
        } else {
            let query = SearchQuery::new(&view.filter, SearchMode::Contains);
            let visible: Vec<usize> = view
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| query.is_empty() || query.matches(&entry.name, None))
                .map(|(index, _)| index)
                .collect();
            if visible.is_empty() {
                ui.add_space(30.0);
                empty_state(
                    ui,
                    Icon::Search,
                    tr("Нет совпадений"),
                    tr("Измените фильтр внутри архива"),
                );
            } else {
                let table = TableBuilder::new(ui)
                    .id_salt("archive_table")
                    .striped(self.striped_rows)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .sense(egui::Sense::click())
                    .column(Column::remainder().at_least(220.0))
                    .column(Column::exact(90.0))
                    .column(Column::exact(80.0));
                table
                    .header(28.0, |mut header| {
                        header.col(|ui| {
                            header_label(ui, tr("Имя"));
                        });
                        header.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    header_label(ui, tr("Размер"));
                                },
                            );
                        });
                        header.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    header_label(ui, tr("Тип"));
                                },
                            );
                        });
                    })
                    .body(|body| {
                        body.rows(30.0, visible.len(), |mut row| {
                            let entry = &view.entries[visible[row.index()]];
                            row.set_selected(
                                view.selected.as_deref() == Some(entry.inner.as_str()),
                            );
                            row.col(|ui| {
                                name_cell(ui, entry.is_dir, entry.color, &entry.name);
                            });
                            row.col(|ui| {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(entry.size_text())
                                                .size(10.5)
                                                .color(TEXT_MUTED),
                                        );
                                    },
                                );
                            });
                            row.col(|ui| {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(tr(entry.kind))
                                                .size(10.5)
                                                .color(TEXT_FAINT),
                                        );
                                    },
                                );
                            });
                            let response = row.response();
                            if response.double_clicked() {
                                selected_request = Some(entry.inner.clone());
                                open = Some(entry.inner.clone());
                            } else if response.clicked() {
                                selected_request = Some(entry.inner.clone());
                            }
                            let entry_inner = entry.inner.clone();
                            let entry_is_dir = entry.is_dir;
                            let entry_path = entry.path.display().to_string();
                            response.context_menu(|ui| {
                                if ui.button(tr("Открыть")).clicked() {
                                    selected_request = Some(entry_inner.clone());
                                    open = Some(entry_inner.clone());
                                    ui.close_menu();
                                }
                                if !entry_is_dir && ui.button(tr("Извлечь…")).clicked() {
                                    extract = Some(entry_inner.clone());
                                    ui.close_menu();
                                }
                                if ui.button(tr("Копировать путь")).clicked() {
                                    copy_path = Some(entry_path.clone());
                                    ui.close_menu();
                                }
                            });
                        });
                    });
            }
        }

        if let Some(inner) = selected_request {
            if let Some(view) = self.archive.as_mut() {
                view.selected = Some(inner);
            }
        }
        if let Some(path) = copy_path {
            ui.ctx().copy_text(path);
            self.status = tr("Путь скопирован").to_owned();
        }

        if close {
            self.close_archive();
            return;
        }
        if back {
            self.archive_back();
            return;
        }
        if up {
            self.archive_up();
            return;
        }
        if reload {
            self.archive_reload();
            return;
        }
        if let Some(inner) = navigate {
            self.archive_navigate(inner);
            return;
        }
        if let Some(inner) = open {
            self.archive_open_inner(inner);
            return;
        }
        if let Some(inner) = extract {
            self.archive_extract_inner(&inner);
            return;
        }
        if extract_all {
            self.archive_extract_all();
        }
    }

    fn draw_central(&mut self, ui: &mut egui::Ui) {
        egui::Frame::NONE
            .fill(glass_soft())
            .stroke(egui::Stroke::new(1.0_f32, glass_border()))
            .corner_radius(12)
            .shadow(egui::epaint::Shadow {
                offset: [0, 10],
                blur: 26,
                spread: 0,
                color: egui::Color32::from_black_alpha(110),
            })
            .outer_margin(egui::Margin::same(6))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                if self.archive.is_some() {
                    self.draw_archive(ui);
                    return;
                }
                let searching = !self.search_results.is_empty()
                    || self.search_state.is_some()
                    || !self.search_summary.is_empty();
                self.draw_central_header(ui, searching);
                ui.add_space(4.0);
                if searching {
                    self.draw_search_results(ui);
                } else if self.entries.is_empty() {
                    ui.add_space(30.0);
                    if self.dir_load.is_some() {
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(tr("Читаю папку…"))
                                    .size(12.0)
                                    .color(TEXT_FAINT),
                            );
                        });
                    } else {
                        empty_state(ui, Icon::Folder, tr("Папка пуста"), tr("Здесь пока нет объектов"));
                    }
                    self.arm_background_menu(ui);
                } else if self.filter_indices.is_empty() {
                    ui.add_space(30.0);
                    empty_state(
                        ui,
                        Icon::Search,
                        tr("Нет совпадений"),
                        tr("Измените запрос в поле поиска"),
                    );
                    self.arm_background_menu(ui);
                } else {
                    match self.view {
                        ViewMode::List => self.draw_file_list(ui),
                        ViewMode::Tiles => self.draw_file_tiles(ui),
                    }
                }
            });
        self.draw_background_menu(ui.ctx());
    }

    fn draw_central_header(&mut self, ui: &mut egui::Ui, searching: bool) {
        let mut sort_changed = false;
        let busy = self.op_state.is_some();
        ui.horizontal(|ui| {
            if searching {
                ui.label(
                    egui::RichText::new(tr("Результаты поиска"))
                        .size(14.0)
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(format!("{}", self.search_results.len()))
                        .size(11.0)
                        .color(TEXT_FAINT),
                );
            } else {
                ui.label(
                    egui::RichText::new(tab_title(&self.current_dir))
                        .size(14.0)
                        .strong()
                        .color(TEXT),
                );
                let count = if self.filter_text.trim().is_empty() {
                    items_text(self.entries.len())
                } else {
                    visible_text(self.filter_indices.len(), self.entries.len())
                };
                ui.label(egui::RichText::new(count).size(10.5).color(TEXT_FAINT));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if searching && icon_button(ui, Icon::Close, tr("Закрыть поиск"), false, true).clicked()
                {
                    self.clear_search();
                }
                ui.menu_button(egui::RichText::new(tr("Действия")).size(11.0), |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Новая папка        Ctrl+Shift+N")))
                        .clicked()
                    {
                        self.create_folder();
                        ui.close_menu();
                    }
                    if ui.button(tr("Перейти к папке…")).clicked() {
                        self.open_picker();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            !self.filter_text.trim().is_empty(),
                            egui::Button::new(tr("Искать в текущей папке…")),
                        )
                        .clicked()
                    {
                        self.start_search(SearchScope::Folder);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Копировать        Ctrl+C")))
                        .clicked()
                    {
                        self.copy_selection(false);
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Вырезать           Ctrl+X")))
                        .clicked()
                    {
                        self.copy_selection(true);
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Вставить            Ctrl+V")))
                        .clicked()
                    {
                        self.paste();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Переименовать              F2")))
                        .clicked()
                    {
                        self.begin_rename();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Удалить в корзину        Del")))
                        .clicked()
                    {
                        self.delete_selection(false);
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(tr("Удалить безвозвратно Shift+Del")),
                        )
                        .clicked()
                    {
                        self.delete_selection(true);
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Отменить                 Ctrl+Z")))
                        .clicked()
                    {
                        self.undo();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(tr("Настройки…")).clicked() {
                        self.settings_open = true;
                        ui.close_menu();
                    }
                });
                if icon_button(
                    ui,
                    Icon::Trash,
                    tr("Удалить в корзину (Del). Shift+Del — безвозвратно"),
                    false,
                    !busy && !self.selection.is_empty(),
                )
                .clicked()
                {
                    self.delete_selection(false);
                }
                if icon_button(ui, Icon::Plus, tr("Новая папка"), false, !busy).clicked() {
                    self.create_folder();
                }
                if icon_button(ui, Icon::Properties, tr("Свойства"), self.show_properties, true)
                    .clicked()
                {
                    self.show_properties = !self.show_properties;
                }
                if !searching {
                    if icon_button(ui, Icon::Grid, tr("Плитка"), self.view == ViewMode::Tiles, true)
                        .clicked()
                    {
                        self.view = ViewMode::Tiles;
                    }
                    if icon_button(ui, Icon::List, tr("Список"), self.view == ViewMode::List, true)
                        .clicked()
                    {
                        self.view = ViewMode::List;
                    }
                    ui.menu_button(
                        egui::RichText::new(tr("Сортировка")).size(11.0),
                        |ui| {
                            sort_changed = sort_menu(ui, &mut self.sort);
                        },
                    );
                }
            });
        });
        if sort_changed {
            self.sort_entries();
            self.rebuild_filter();
        }
    }

    fn draw_search_results(&mut self, ui: &mut egui::Ui) {
        if self.search_state.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(tr("Ищу…")).size(11.0).color(TEXT_FAINT));
            });
            ui.add_space(2.0);
        }
        if self.search_results.is_empty() {
            if self.search_state.is_none() {
                empty_state(
                    ui,
                    Icon::Search,
                    tr("Ничего не найдено"),
                    tr("Попробуйте другой запрос"),
                );
            }
            return;
        }
        let mut open_path: Option<SearchHit> = None;
        let mut open_with: Option<PathBuf> = None;
        let mut open_new_tab: Option<PathBuf> = None;
        let mut clicked_row: Option<(usize, egui::Modifiers)> = None;
        let mut context_row: Option<(usize, egui::Modifiers)> = None;
        let mut copy_paths = false;
        let mut copy_items = false;
        let mut cut_items = false;
        let mut rename = false;
        let mut trash = false;
        let mut permanent = false;
        let mut properties = false;
        let mut create: Option<(CreateKind, PathBuf)> = None;
        let mut shortcut: Option<PathBuf> = None;
        let modifiers = ui.ctx().input(|input| input.modifiers);
        let mut table = TableBuilder::new(ui)
            .id_salt("search_table")
            .striped(self.striped_rows)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .sense(egui::Sense::click())
            .column(Column::initial(260.0).at_least(150.0))
            .column(Column::remainder().at_least(180.0))
            .column(Column::exact(90.0));
        if let Some(row) = self.scroll_to_row.take() {
            table = table.scroll_to_row(row, None);
        }
        table
            .header(28.0, |mut header| {
                header.col(|ui| {
                    header_label(ui, tr("Имя"));
                });
                header.col(|ui| {
                    header_label(ui, tr("Путь"));
                });
                header.col(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        header_label(ui, tr("Тип"));
                    });
                });
            })
            .body(|body| {
                body.rows(30.0, self.search_results.len(), |mut row| {
                    let index = row.index();
                    let hit = &self.search_results[index];
                    row.set_selected(self.selection.contains(&hit.path));
                    row.col(|ui| {
                        name_cell(ui, hit.is_dir, category_color(&hit.path, hit.is_dir), &hit.name);
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(hit.parent_text())
                                    .size(10.5)
                                    .color(TEXT_MUTED),
                            )
                            .truncate(),
                        )
                        .on_hover_text(hit.path.display().to_string());
                    });
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(kind_label(&hit.path, hit.is_dir))
                                    .size(10.5)
                                    .color(TEXT_FAINT),
                            );
                        });
                    });
                    let response = row.response();
                    if response.secondary_clicked() && !self.selection.contains(&hit.path) {
                        context_row = Some((index, modifiers));
                    }
                    if response.double_clicked() {
                        open_path = Some(hit.clone());
                    } else if response.clicked() {
                        clicked_row = Some((index, modifiers));
                    }
                    let menu = row_context_menu(
                        &response,
                        hit.is_dir,
                        &hit.path,
                        hit.is_dir.then_some(hit.path.as_path()),
                    );
                    if menu.open {
                        open_path = Some(hit.clone());
                    }
                    if menu.open_with {
                        open_with = Some(hit.path.clone());
                    }
                    if menu.open_new_tab {
                        open_new_tab = Some(hit.path.clone());
                    }
                    copy_paths |= menu.copy_paths;
                    copy_items |= menu.copy;
                    cut_items |= menu.cut;
                    rename |= menu.rename;
                    trash |= menu.trash;
                    permanent |= menu.permanent;
                    properties |= menu.properties;
                    if menu.create.is_some() {
                        create = menu.create;
                    }
                    if menu.shortcut.is_some() {
                        shortcut = menu.shortcut;
                    }
                });
            });
        if let Some((index, modifiers)) = clicked_row.or(context_row) {
            apply_selection(
                &mut self.selection,
                &mut self.anchor,
                &mut self.selected,
                &self.search_results,
                index,
                modifiers,
            );
            release_text_focus(ui.ctx());
        }
        if copy_paths {
            self.copy_paths_to_clipboard(ui.ctx());
        }
        if copy_items {
            self.copy_selection(false);
        }
        if cut_items {
            self.copy_selection(true);
        }
        if properties {
            self.show_properties = true;
        }
        if rename {
            self.begin_rename();
        }
        if trash {
            self.delete_selection(false);
        }
        if permanent {
            self.delete_selection(true);
        }
        if let Some((kind, dir)) = create {
            self.create_in(kind, &dir);
        }
        if let Some(path) = shortcut {
            self.create_shortcut(&path);
        }
        if let Some(path) = open_new_tab {
            self.new_tab(path);
        }
        if let Some(hit) = open_path {
            self.open_path_item(hit.path, hit.is_dir);
        }
        if let Some(path) = open_with {
            if let Err(err) = open_with_dialog(&path) {
                self.status = tr_msg(
                format!("Не удалось открыть с помощью: {err}"),
                format!("Could not open with: {err}"),
            );
            }
        }
    }

    fn draw_file_list(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<PathBuf> = None;
        let mut open_with: Option<PathBuf> = None;
        let mut open_new_tab: Option<PathBuf> = None;
        let mut clicked_row: Option<(usize, egui::Modifiers)> = None;
        let mut context_row: Option<(usize, egui::Modifiers)> = None;
        let mut copy_paths = false;
        let mut copy_items = false;
        let mut cut_items = false;
        let mut rename = false;
        let mut trash = false;
        let mut permanent = false;
        let mut properties = false;
        let mut sort_changed = false;
        let mut create: Option<(CreateKind, PathBuf)> = None;
        let mut shortcut: Option<PathBuf> = None;
        let mut row_secondary = false;
        let modifiers = ui.ctx().input(|input| input.modifiers);
        let row_height = if self.compact_rows { 26.0 } else { 30.0 };
        let mut table = TableBuilder::new(ui)
            .id_salt("files_table")
            .striped(self.striped_rows)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .sense(egui::Sense::click())
            .column(Column::remainder().at_least(130.0))
            .column(Column::exact(70.0))
            .column(Column::exact(90.0))
            .column(Column::exact(110.0));
        if let Some(row) = self.scroll_to_row.take() {
            table = table.scroll_to_row(row, None);
        }
        table
            .header(28.0, |mut header| {
                header.col(|ui| {
                    sort_changed |= sort_header(ui, tr("Имя"), SortKey::Name, &mut self.sort);
                });
                header.col(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        sort_changed |= sort_header(ui, tr("Размер"), SortKey::Size, &mut self.sort);
                    });
                });
                header.col(|ui| {
                    sort_changed |= sort_header(ui, tr("Тип"), SortKey::Kind, &mut self.sort);
                });
                header.col(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        sort_changed |=
                            sort_header(ui, tr("Изменён"), SortKey::Modified, &mut self.sort);
                    });
                });
            })
            .body(|body| {
                body.rows(row_height, self.filter_indices.len(), |mut row| {
                    let row_index = row.index();
                    let Some(&index) = self.filter_indices.get(row_index) else {
                        return;
                    };
                    let Some(entry) = self.entries.get(index) else {
                        return;
                    };
                    row.set_selected(self.selection.contains(&entry.path));
                    row.col(|ui| {
                        name_cell(ui, entry.is_dir, entry.color, &entry.name);
                    });
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if !entry.is_dir {
                                ui.label(
                                    egui::RichText::new(entry.size_text())
                                        .size(11.0)
                                        .color(TEXT_MUTED),
                                );
                            }
                        });
                    });
                    row.col(|ui| {
                        ui.label(egui::RichText::new(tr(entry.kind)).size(10.5).color(TEXT_FAINT));
                    });
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(
                                    entry
                                        .modified_text
                                        .get_or_init(|| format_modified(entry.modified)),
                                )
                                .size(10.5)
                                .color(TEXT_FAINT),
                            );
                        });
                    });
                    let response = row.response();
                    if response.secondary_clicked() {
                        row_secondary = true;
                        if !self.selection.contains(&entry.path) {
                            context_row = Some((row_index, modifiers));
                        }
                    }
                    if response.double_clicked() {
                        action = Some(entry.path.clone());
                    } else if response.clicked() {
                        clicked_row = Some((row_index, modifiers));
                    }
                    let menu = row_context_menu(
                        &response,
                        entry.is_dir,
                        &entry.path,
                        entry.is_dir.then_some(entry.path.as_path()),
                    );
                    if menu.open {
                        action = Some(entry.path.clone());
                    }
                    if menu.open_with {
                        open_with = Some(entry.path.clone());
                    }
                    if menu.open_new_tab {
                        open_new_tab = Some(entry.path.clone());
                    }
                    copy_paths |= menu.copy_paths;
                    copy_items |= menu.copy;
                    cut_items |= menu.cut;
                    rename |= menu.rename;
                    trash |= menu.trash;
                    permanent |= menu.permanent;
                    properties |= menu.properties;
                    if menu.create.is_some() {
                        create = menu.create;
                    }
                    if menu.shortcut.is_some() {
                        shortcut = menu.shortcut;
                    }
                });
            });
        if sort_changed {
            self.sort_entries();
            self.rebuild_filter();
        }
        if let Some((row_index, modifiers)) = clicked_row.or(context_row) {
            apply_selection_visible(
                &mut self.selection,
                &mut self.anchor,
                &mut self.selected,
                &self.entries,
                &self.filter_indices,
                row_index,
                modifiers,
            );
            // Иначе фокус остаётся в поле поиска и Del/Delete не доходит
            // до списка файлов (а печатается в строку поиска).
            release_text_focus(ui.ctx());
        }
        if row_secondary {
            self.background_menu = None;
        } else {
            self.arm_background_menu(ui);
        }
        if copy_paths {
            self.copy_paths_to_clipboard(ui.ctx());
        }
        if copy_items {
            self.copy_selection(false);
        }
        if cut_items {
            self.copy_selection(true);
        }
        if properties {
            self.show_properties = true;
        }
        if rename {
            self.begin_rename();
        }
        if trash {
            self.delete_selection(false);
        }
        if permanent {
            self.delete_selection(true);
        }
        if let Some((kind, dir)) = create {
            self.create_in(kind, &dir);
        }
        if let Some(path) = shortcut {
            self.create_shortcut(&path);
        }
        if let Some(path) = open_new_tab {
            self.new_tab(path);
        }
        if let Some(path) = action {
            let is_dir = path_is_dir(&path);
            self.open_path_item(path, is_dir);
        }
        if let Some(path) = open_with {
            if let Err(err) = open_with_dialog(&path) {
                self.status = tr_msg(
                format!("Не удалось открыть с помощью: {err}"),
                format!("Could not open with: {err}"),
            );
            }
        }
    }

    fn draw_file_tiles(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_width();
        let tile_width = 160.0_f32;
        let gap = 8.0_f32;
        let tile_height = if self.compact_rows { 88.0 } else { 100.0 };
        let row_height = tile_height + gap;
        let columns = (((available + gap) / (tile_width + gap)).floor() as usize).max(1);
        let rows = self.filter_indices.len().div_ceil(columns);
        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(row) = self.scroll_to_row.take() {
            let row = row / columns;
            scroll = scroll.vertical_scroll_offset(row as f32 * row_height);
        }
        let mut action: Option<PathBuf> = None;
        let mut open_with: Option<PathBuf> = None;
        let mut open_new_tab: Option<PathBuf> = None;
        let mut clicked_row: Option<(usize, egui::Modifiers)> = None;
        let mut context_row: Option<(usize, egui::Modifiers)> = None;
        let mut copy_paths = false;
        let mut copy_items = false;
        let mut cut_items = false;
        let mut rename = false;
        let mut trash = false;
        let mut permanent = false;
        let mut properties = false;
        let mut create: Option<(CreateKind, PathBuf)> = None;
        let mut shortcut: Option<PathBuf> = None;
        let mut tile_secondary = false;
        let entries = &self.entries;
        let visible = &self.filter_indices;
        let selection = &self.selection;
        scroll.show_rows(ui, row_height, rows, |ui, row_range| {
            for row in row_range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for column in 0..columns {
                        let visible_index = row * columns + column;
                        let Some(&index) = visible.get(visible_index) else {
                            continue;
                        };
                        let Some(entry) = entries.get(index) else {
                            continue;
                        };
                        let selected = selection.contains(&entry.path);
                        let response = paint_tile(ui, entry, selected, tile_width, tile_height);
                        if response.secondary_clicked() {
                            tile_secondary = true;
                            if !selected {
                                context_row = Some((
                                    visible_index,
                                    ui.ctx().input(|input| input.modifiers),
                                ));
                            }
                        }
                        if response.double_clicked() {
                            action = Some(entry.path.clone());
                        } else if response.clicked() {
                            clicked_row =
                                Some((visible_index, ui.ctx().input(|input| input.modifiers)));
                        }
                        let menu = row_context_menu(
                            &response,
                            entry.is_dir,
                            &entry.path,
                            entry.is_dir.then_some(entry.path.as_path()),
                        );
                        if menu.open {
                            action = Some(entry.path.clone());
                        }
                        if menu.open_with {
                            open_with = Some(entry.path.clone());
                        }
                        if menu.open_new_tab {
                            open_new_tab = Some(entry.path.clone());
                        }
                        copy_paths |= menu.copy_paths;
                        copy_items |= menu.copy;
                        cut_items |= menu.cut;
                        rename |= menu.rename;
                        trash |= menu.trash;
                        permanent |= menu.permanent;
                        properties |= menu.properties;
                        if menu.create.is_some() {
                            create = menu.create;
                        }
                        if menu.shortcut.is_some() {
                            shortcut = menu.shortcut;
                        }
                    }
                });
            }
        });
        if let Some((visible_index, modifiers)) = clicked_row.or(context_row) {
            apply_selection_visible(
                &mut self.selection,
                &mut self.anchor,
                &mut self.selected,
                &self.entries,
                &self.filter_indices,
                visible_index,
                modifiers,
            );
            release_text_focus(ui.ctx());
        }
        if tile_secondary {
            self.background_menu = None;
        } else {
            self.arm_background_menu(ui);
        }
        if copy_paths {
            self.copy_paths_to_clipboard(ui.ctx());
        }
        if copy_items {
            self.copy_selection(false);
        }
        if cut_items {
            self.copy_selection(true);
        }
        if properties {
            self.show_properties = true;
        }
        if rename {
            self.begin_rename();
        }
        if trash {
            self.delete_selection(false);
        }
        if permanent {
            self.delete_selection(true);
        }
        if let Some((kind, dir)) = create {
            self.create_in(kind, &dir);
        }
        if let Some(path) = shortcut {
            self.create_shortcut(&path);
        }
        if let Some(path) = open_new_tab {
            self.new_tab(path);
        }
        if let Some(path) = action {
            let is_dir = path_is_dir(&path);
            self.open_path_item(path, is_dir);
        }
        if let Some(path) = open_with {
            if let Err(err) = open_with_dialog(&path) {
                self.status = tr_msg(
                format!("Не удалось открыть с помощью: {err}"),
                format!("Could not open with: {err}"),
            );
            }
        }
    }

    fn apply_visual_settings(&self, ctx: &egui::Context) {
        let target = self.ui_scale.clamp(0.8, 1.35);
        // Плавно подводим масштаб к целевому значению, а не прыгаем кадром.
        let scale = if self.animations {
            ctx.animate_value_with_time(egui::Id::new("korobka_ui_scale"), target, 0.18)
        } else {
            target
        };
        if (ctx.zoom_factor() - scale).abs() > 0.001 {
            ctx.set_zoom_factor(scale);
        }
        let animation = if self.animations { 0.15 } else { 0.0 };
        if (ctx.style().animation_time - animation).abs() > 0.001 {
            ctx.style_mut(|style| style.animation_time = animation);
        }
    }

    fn load_trash(&mut self) {
        self.trash_items.clear();
        self.trash_error.clear();
        let root = fs_ops::trash_root();
        if !crate::path_util::extended(&root).exists() {
            self.trash_selected = None;
            return;
        }
        let batches = match fs::read_dir(crate::path_util::extended(&root)) {
            Ok(batches) => batches,
            Err(error) => {
                self.trash_error = tr_msg(
                    format!("Не удалось прочитать корзину: {error}"),
                    format!("Could not read trash: {error}"),
                );
                return;
            }
        };
        let mut items = Vec::new();
        for batch in batches.flatten() {
            let Ok(file_type) = batch.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let batch_path = crate::path_util::normalized(&batch.path());
            let manifest = fs_ops::read_trash_manifest(&batch_path);
            let deleted = batch.metadata().ok().and_then(|meta| meta.modified().ok());
            let Ok(entries) = fs::read_dir(crate::path_util::extended(&batch_path)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = crate::path_util::normalized(&entry.path());
                let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
                let original = manifest
                    .iter()
                    .find(|record| record.trashed == path)
                    .map(|record| record.original.clone());
                items.push(TrashItem {
                    path,
                    original,
                    batch: batch_path.clone(),
                    name,
                    is_dir,
                    deleted,
                });
            }
        }
        items.sort_by(|a, b| {
            b.deleted
                .cmp(&a.deleted)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.trash_items = items;
        if let Some(selected) = &self.trash_selected {
            if !self.trash_items.iter().any(|item| &item.path == selected) {
                self.trash_selected = None;
            }
        }
    }

    fn restore_trash_item(&mut self, path: &Path) {
        let Some(item) = self
            .trash_items
            .iter()
            .find(|item| item.path == path)
            .cloned()
        else {
            return;
        };
        let target = item
            .original
            .clone()
            .unwrap_or_else(|| self.current_dir.join(&item.name));
        let target = fs_ops::available_path(&target);
        match fs_ops::move_item(&item.path, &target) {
            Ok(()) => {
                fs_ops::cleanup_trash_batch(&item.batch);
                self.invalidate_index();
                self.refresh_directory();
                self.load_trash();
                self.status = tr_msg(
                    format!("Восстановлено: {}", crate::path_util::display(&target)),
                    format!("Restored: {}", crate::path_util::display(&target)),
                );
            }
            Err(error) => {
                self.status = tr_msg(
                    format!("Не удалось восстановить: {error}"),
                    format!("Could not restore: {error}"),
                );
            }
        }
    }

    fn draw_trash(&mut self, ctx: &egui::Context) {
        if !self.trash_open {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.trash_open = false;
            self.trash_selected = None;
            return;
        }
        let mut open = true;
        let mut selected = self.trash_selected.clone();
        let mut refresh = false;
        let mut restore = false;
        let mut delete = false;
        let mut empty = false;
        egui::Window::new(tr("Корзина"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(760.0)
            .show(ctx, |ui| {
                ui.set_min_width(620.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(items_text(self.trash_items.len()))
                            .size(11.0)
                            .color(TEXT_FAINT),
                    );
                    if ui.small_button(tr("Обновить")).clicked() {
                        refresh = true;
                    }
                    if ui
                        .add_enabled(
                            !self.trash_items.is_empty(),
                            egui::Button::new(tr("Очистить корзину")),
                        )
                        .clicked()
                    {
                        empty = true;
                    }
                });
                if !self.trash_error.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(224, 120, 120), &self.trash_error);
                }
                ui.separator();
                if self.trash_items.is_empty() {
                    empty_state(
                        ui,
                        Icon::Trash,
                        tr("Корзина пуста"),
                        tr("Удалённые объекты появятся здесь"),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .show(ui, |ui| {
                            for item in &self.trash_items {
                                let original = item
                                    .original
                                    .as_ref()
                                    .map(|path| crate::path_util::display(path))
                                    .unwrap_or_else(|| tr("Исходный путь неизвестен").to_owned());
                                let kind = if item.is_dir {
                                    tr("Папка")
                                } else {
                                    tr("Файл")
                                };
                                let label = format!("{} · {}\n{}", item.name, kind, original);
                                if ui
                                    .selectable_label(
                                        selected.as_ref() == Some(&item.path),
                                        label,
                                    )
                                    .clicked()
                                {
                                    selected = Some(item.path.clone());
                                }
                            }
                        });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            selected.is_some(),
                            egui::Button::new(tr("Восстановить")),
                        )
                        .clicked()
                    {
                        restore = true;
                    }
                    if ui
                        .add_enabled(
                            selected.is_some(),
                            egui::Button::new(tr("Удалить безвозвратно")),
                        )
                        .clicked()
                    {
                        delete = true;
                    }
                });
            });
        self.trash_open = open;
        self.trash_selected = selected;
        if refresh {
            self.load_trash();
        }
        if restore {
            if let Some(path) = self.trash_selected.clone() {
                self.restore_trash_item(&path);
            }
        }
        if delete {
            if let Some(path) = self.trash_selected.clone() {
                self.trash_delete_pending = Some(vec![path]);
            }
        }
        if empty {
            let paths: Vec<PathBuf> = self
                .trash_items
                .iter()
                .map(|item| item.path.clone())
                .collect();
            if !paths.is_empty() {
                self.trash_delete_pending = Some(paths);
            }
        }
    }

    fn draw_trash_confirm(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.trash_delete_pending.clone() else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        let mut cancel = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Window::new(tr("Удалить безвозвратно?"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(if i18n::english() {
                    format!("Items to delete permanently: {}", paths.len())
                } else {
                    format!("Будет безвозвратно удалено объектов: {}", paths.len())
                });
                for path in paths.iter().take(3) {
                    ui.label(
                        egui::RichText::new(crate::path_util::display(path))
                            .size(10.5)
                            .monospace()
                            .color(TEXT_MUTED),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("Удалить")).clicked() {
                        confirm = true;
                    }
                    if ui.button(tr("Отмена")).clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            let mut errors = Vec::new();
            let batches: Vec<PathBuf> = self
                .trash_items
                .iter()
                .filter(|item| paths.contains(&item.path))
                .map(|item| item.batch.clone())
                .collect();
            for path in &paths {
                if let Err(error) = fs_ops::remove_any(path) {
                    errors.push(error.to_string());
                }
            }
            for batch in batches {
                fs_ops::cleanup_trash_batch(&batch);
            }
            self.invalidate_index();
            self.refresh_directory();
            self.trash_delete_pending = None;
            self.load_trash();
            self.status = if errors.is_empty() {
                tr_msg(
                    format!("Удалено безвозвратно: {}", paths.len()),
                    format!("Deleted permanently: {}", paths.len()),
                )
            } else {
                tr_msg(
                    format!("Ошибка удаления: {}", errors.join("; ")),
                    format!("Delete error: {}", errors.join("; ")),
                )
            };
        } else if cancel || !open {
            self.trash_delete_pending = None;
        }
    }

    fn draw_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = self.settings_open;
        let appearance_before = self.appearance;
        let accent_before = self.accent;
        let language_before = self.language;
        let blur_before = self.glass_blur;
        let transparency_before = self.glass_transparency;
        let mut folders_changed = false;
        let mut hidden_changed = false;
        let mut reset = false;
        let mut save_pressed = false;
        let settings_before = serde_json::to_string(&session_state(self)).ok();
        egui::Window::new(tr("Настройки"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .vscroll(true)
            .default_width(380.0)
            .default_height(520.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(tr("Вид")).size(11.0).color(TEXT_FAINT));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.view, ViewMode::List, tr("Список"));
                    ui.selectable_value(&mut self.view, ViewMode::Tiles, tr("Плитка"));
                });
                ui.checkbox(&mut self.compact_rows, tr("Компактные строки"));
                ui.checkbox(&mut self.striped_rows, tr("Полосатые строки"));
                if ui
                    .checkbox(&mut self.folders_first, tr("Папки сверху"))
                    .changed()
                {
                    folders_changed = true;
                }
                if ui
                    .checkbox(
                        &mut self.show_hidden,
                        tr("Показывать скрытые и системные"),
                    )
                    .changed()
                {
                    hidden_changed = true;
                }

                ui.add_space(6.0);
                ui.label(egui::RichText::new(tr("Оформление")).size(11.0).color(TEXT_FAINT));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.appearance, Appearance::Matte, tr("Матовый"));
                    ui.selectable_value(&mut self.appearance, Appearance::Glass, tr("Стекло"));
                });
                ui.horizontal(|ui| {
                    ui.label(tr("Акцент"));
                    let _ = ui.color_edit_button_srgb(&mut self.accent);
                    if ui.small_button(tr("Сброс")).clicked() {
                        self.accent = [0x4D, 0xA3, 0xE8];
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(tr("Язык"));
                    ui.selectable_value(
                        &mut self.language,
                        Language::System,
                        tr("Системный"),
                    );
                    ui.selectable_value(
                        &mut self.language,
                        Language::Russian,
                        tr("Русский"),
                    );
                    ui.selectable_value(
                        &mut self.language,
                        Language::English,
                        "English",
                    );
                });
                ui.label(
                    egui::RichText::new(tr("Язык определяется по Windows."))
                        .size(10.0)
                        .color(TEXT_FAINT),
                );
                ui.checkbox(&mut self.animations, tr("Анимации"));
                ui.checkbox(&mut self.show_properties, tr("Показывать панель свойств"));
                ui.checkbox(&mut self.sidebar_collapsed, tr("Свернуть боковую панель"));
                ui.horizontal(|ui| {
                    ui.label(tr("Масштаб"));
                    ui.add(
                        egui::Slider::new(&mut self.ui_scale, 0.8..=1.35)
                            .fixed_decimals(2),
                    );
                    if ui.small_button("100%").clicked() {
                        self.ui_scale = 1.0;
                    }
                });
                if self.appearance == Appearance::Glass {
                    ui.checkbox(&mut self.glass_blur, tr("Размытие фона (акрил Windows)"));
                    ui.horizontal(|ui| {
                        ui.label(tr("Прозрачность"));
                        ui.add(
                            egui::Slider::new(&mut self.glass_transparency, 0..=100)
                                .suffix("%"),
                        );
                    });
                }
                if self.appearance == Appearance::Glass
                    && self.glass_blur
                    && !self.backdrop_ready
                {
                    ui.label(
                        egui::RichText::new(
                            tr("Акрил Windows недоступен — стекло работает как полупрозрачный слой."),
                        )
                        .size(10.0)
                        .color(TEXT_FAINT),
                    );
                }
                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.confirm_permanent_delete,
                    tr("Подтверждать безвозвратное удаление"),
                );

                ui.separator();
                ui.label(egui::RichText::new(tr("Поиск")).size(11.0).color(TEXT_FAINT));
                ui.horizontal(|ui| {
                    ui.label(tr("Потоки поиска"));
                    ui.add(egui::Slider::new(&mut self.search_threads, 2..=16));
                });
                ui.label(
                    egui::RichText::new(tr(
                        "Меньше потоков — ниже нагрузка на CPU, но индекс строится дольше.",
                    ))
                    .size(10.0)
                    .color(TEXT_FAINT),
                );
                if ui.button(tr("Сбросить индекс поиска")).clicked() {
                    self.invalidate_index();
                    self.status = tr("Индекс сброшен").to_owned();
                }
                ui.label(
                    egui::RichText::new(tr("Индекс пересоздастся при следующем поиске."))
                        .size(10.5)
                        .color(TEXT_FAINT),
                );

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(tr("Сохранить настройки")).clicked() {
                        save_pressed = true;
                    }
                    if ui.button(tr("Сбросить настройки")).clicked() {
                        reset = true;
                    }
                });
                ui.label(
                    egui::RichText::new(tr("Вкладки и открытые папки не сбрасываются."))
                        .size(10.0)
                        .color(TEXT_FAINT),
                );
                if let Some(path) = session_path() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string())
                                .size(9.5)
                                .color(TEXT_DISABLED),
                        )
                        .truncate(),
                    );
                }
            });
        self.settings_open = open;
        if folders_changed {
            self.sort_entries();
            self.rebuild_filter();
        }
        if hidden_changed {
            self.dir_cache.clear();
            self.refresh_directory();
        }
        if reset {
            self.view = ViewMode::List;
            self.compact_rows = false;
            self.striped_rows = false;
            self.folders_first = true;
            self.show_hidden = true;
            self.appearance = Appearance::Matte;
            self.accent = [0x4D, 0xA3, 0xE8];
            self.glass_blur = true;
            self.glass_transparency = 65;
            self.animations = true;
            self.confirm_permanent_delete = true;
            self.show_properties = false;
            self.sidebar_collapsed = false;
            self.ui_scale = 1.0;
            self.search_threads = 8;
            self.sort = SortState::default();
            self.sort_entries();
            self.rebuild_filter();
            self.dir_cache.clear();
            self.refresh_directory();
            self.status = tr("Настройки сброшены").to_owned();
        }
        let appearance_changed = self.appearance != appearance_before;
        let accent_changed = self.accent != accent_before;
        let language_changed = self.language != language_before;
        let blur_changed = self.glass_blur != blur_before;
        let transparency_changed = self.glass_transparency != transparency_before;
        if language_changed {
            set_english(language_is_english(self.language));
            self.places = quick_places();
            ctx.request_repaint();
        }
        if accent_changed {
            set_accent(self.accent);
        }
        if appearance_changed {
            set_real_glass(self.appearance == Appearance::Glass);
        }
        if appearance_changed || blur_changed || transparency_changed {
            set_glass_transparency(self.glass_transparency);
            self.backdrop_ready = apply_windows_backdrop(
                self.appearance,
                self.glass_blur,
                self.glass_transparency,
            );
        }
        if appearance_changed || accent_changed || blur_changed || transparency_changed {
            apply_visual_colors(ctx);
            ctx.request_repaint();
        }
        self.apply_visual_settings(ctx);

        let settings_after = serde_json::to_string(&session_state(self)).ok();
        let changed = settings_before != settings_after;
        if save_pressed || !open {
            save_session(self);
            self.settings_save_at = None;
            if save_pressed {
                self.status = tr("Настройки сохранены").to_owned();
            }
        } else if changed {
            self.settings_save_at = Some(Instant::now());
        }
    }

    fn flush_settings_save(&mut self, ctx: &egui::Context) {
        let Some(at) = self.settings_save_at else {
            return;
        };
        let delay = Duration::from_millis(400);
        if at.elapsed() >= delay {
            save_session(self);
            self.settings_save_at = None;
        } else {
            ctx.request_repaint_after(delay.saturating_sub(at.elapsed()));
        }
    }

    fn open_picker(&mut self) {
        self.picker_dir = self.current_dir.clone();
        self.picker_path_text = self.picker_dir.display().to_string();
        self.picker_open = true;
        self.picker_error.clear();
        if cfg!(windows) && !self.drives_loaded {
            self.drives = drives::enumerate();
            self.drives_loaded = true;
        }
        self.load_picker_dirs();
    }

    fn load_picker_dirs(&mut self) {
        let path = clean_path(&self.picker_path_text);
        if !path_is_dir(&path) {
            self.picker_error = tr("Такой папки нет или доступ запрещён").to_owned();
            return;
        }
        self.picker_dir = path.clone();
        self.picker_path_text = path.display().to_string();
        let mut dirs = Vec::new();
        match fs::read_dir(crate::path_util::extended(&path)) {
            Ok(items) => {
                for item in items.flatten() {
                    if item.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                        dirs.push(crate::path_util::normalized(&item.path()));
                    }
                }
                dirs.sort_by_cached_key(|path| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase()
                });
                self.picker_dirs = dirs;
                self.picker_error.clear();
            }
            Err(err) => self.picker_error = tr_msg(
                format!("Не удалось прочитать папку: {err}"),
                format!("Could not read folder: {err}"),
            ),
        }
    }

    fn draw_picker(&mut self, ctx: &egui::Context) {
        if !self.picker_open {
            return;
        }
        let mut is_open = self.picker_open;
        egui::Window::new(tr("Выбор папки Korobka"))
            .open(&mut is_open)
            .default_width(620.0)
            .default_height(480.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("Путь:"));
                    let response = ui.text_edit_singleline(&mut self.picker_path_text);
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.load_picker_dirs();
                    }
                    if ui.button(tr("Перейти")).clicked() {
                        self.load_picker_dirs();
                    }
                    if ui.button(tr("Вверх")).clicked() {
                        if let Some(parent) = self.picker_dir.parent() {
                            self.picker_path_text = parent.display().to_string();
                            self.load_picker_dirs();
                        }
                    }
                });

                #[cfg(windows)]
                {
                    let mut drives_click: Option<PathBuf> = None;
                    let mut refresh_drives = false;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr("Диски:"));
                        for drive in &self.drives {
                            let response = ui.small_button(drive.caption());
                            if response.clicked() {
                                drives_click = Some(drive.root.clone());
                            }
                            response.on_hover_text(if i18n::english() {
                                format!(
                                    "{} · {} free of {}",
                                    tr(drive.kind),
                                    format_size(drive.free),
                                    format_size(drive.total)
                                )
                            } else {
                                format!(
                                    "{} · свободно {} из {}",
                                    drive.kind,
                                    format_size(drive.free),
                                    format_size(drive.total)
                                )
                            });
                        }
                        if ui
                            .small_button(tr("Обновить"))
                            .on_hover_text(tr("Обновить список дисков"))
                            .clicked()
                        {
                            refresh_drives = true;
                        }
                    });
                    if refresh_drives {
                        self.drives = drives::enumerate();
                        self.drives_loaded = true;
                    }
                    if let Some(root) = drives_click {
                        self.picker_path_text = root.display().to_string();
                        self.load_picker_dirs();
                    }
                }

                ui.separator();
                ui.label(if i18n::english() {
                    format!("Current folder: {}", self.picker_dir.display())
                } else {
                    format!("Текущая папка: {}", self.picker_dir.display())
                });
                if !self.picker_error.is_empty() {
                    ui.colored_label(egui::Color32::LIGHT_RED, &self.picker_error);
                }

                let mut open_index = None;
                let row_height = ui.text_style_height(&egui::TextStyle::Body);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, self.picker_dirs.len(), |ui, range| {
                        for index in range {
                            let Some(dir) = self.picker_dirs.get(index) else {
                                continue;
                            };
                            let name = dir.file_name().unwrap_or_default().to_string_lossy();
                            if ui.selectable_label(false, name.as_ref()).double_clicked() {
                                open_index = Some(index);
                            }
                        }
                    });
                if let Some(index) = open_index {
                    if let Some(dir) = self.picker_dirs.get(index).cloned() {
                        self.picker_path_text = dir.display().to_string();
                        self.load_picker_dirs();
                    }
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(tr("Выбрать эту папку")).clicked() {
                        let path = self.picker_dir.clone();
                        self.picker_open = false;
                        self.navigate_to(path);
                    }
                    if ui.button(tr("Отмена")).clicked() {
                        self.picker_open = false;
                    }
                });
            });
        self.picker_open = is_open && self.picker_open;
        if !self.picker_open {
            release_text_focus(ctx);
        }
    }
}

impl Default for KorobkaApp {
    fn default() -> Self {
        let start = if Path::new(r"C:\").is_dir() {
            PathBuf::from(r"C:\")
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };
        Self {
            current_dir: start.clone(),
            path_text: start.display().to_string(),
            entries: Vec::new(),
            filter_text: String::new(),
            filter_indices: Vec::new(),
            filter_dirty: false,
            scroll_to_row: None,
            dir_load: None,
            dir_cache: Vec::new(),
            selected: None,
            selection: BTreeSet::new(),
            anchor: None,
            status: "Готово".to_owned(),
            search_mode_index: 1,
            search_threads: 8,
            search_results: Vec::new(),
            search_state: None,
            last_search_scope: None,
            search_summary: String::new(),
            archive: None,
            clipboard: None,
            rename: None,
            lock_flow: None,
            lock_dialog: None,
            op_state: None,
            op_conflict: None,
            op_apply_all: false,
            op_progress: String::new(),
            undo_stack: Vec::new(),
            picker_open: false,
            picker_dir: PathBuf::new(),
            picker_path_text: String::new(),
            picker_dirs: Vec::new(),
            picker_error: String::new(),
            drives: drives::enumerate(),
            drives_loaded: true,
            sort: SortState::default(),
            history: Vec::new(),
            history_pos: 0,
            places: quick_places(),
            style_ready: false,
            tabs: Vec::new(),
            active_tab: 0,
            view: ViewMode::List,
            sidebar_collapsed: false,
            sidebar_width: 176.0,
            show_properties: false,
            address_editing: false,
            focus_address: false,
            focus_search: false,
            background_menu: None,
            settings_open: false,
            settings_save_at: None,
            trash_open: false,
            trash_items: Vec::new(),
            trash_selected: None,
            trash_error: String::new(),
            trash_delete_pending: None,
            compact_rows: false,
            appearance: Appearance::Matte,
            accent: [0x4D, 0xA3, 0xE8],
            language: Language::System,
            glass_blur: true,
            glass_transparency: 65,
            folders_first: true,
            show_hidden: true,
            striped_rows: false,
            ui_scale: 1.0,
            animations: true,
            confirm_permanent_delete: true,
            pending_permanent_delete: None,
            backdrop_ready: false,
            backdrop_attempted: false,
        }
    }
}

impl Drop for KorobkaApp {
    fn drop(&mut self) {
        if let Some(state) = &self.search_state {
            state.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(state) = &self.op_state {
            state.cancel.store(true, Ordering::Relaxed);
        }
        save_session(self);
    }
}

fn filter_matches(query: &SearchQuery, entry: &FsEntry) -> bool {
    if query.needs_path() {
        query.matches(&entry.name, Some(entry.path.to_string_lossy().as_ref()))
    } else {
        query.matches(&entry.name, None)
    }
}

fn unique_child_name(dir: &Path, base: &str, extension: Option<&str>) -> String {
    for counter in 1..10_000 {
        let name = match (counter, extension) {
            (1, Some(extension)) => format!("{base}.{extension}"),
            (1, None) => base.to_owned(),
            (counter, Some(extension)) => format!("{base} ({counter}).{extension}"),
            (counter, None) => format!("{base} ({counter})"),
        };
        if !path_exists(&dir.join(&name)) {
            return name;
        }
    }
    base.to_owned()
}

fn zip_io_error(error: zip::result::ZipError) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

const DOCX_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const DOCX_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const DOCX_DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p/><w:sectPr/></w:body></w:document>"#;

fn write_docx(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;

    let file = fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = || {
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
    };
    for (name, content) in [
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_RELS),
        ("word/document.xml", DOCX_DOCUMENT),
    ] {
        zip.start_file(name, options()).map_err(zip_io_error)?;
        zip.write_all(content.as_bytes())?;
    }
    zip.finish().map_err(zip_io_error)?;
    Ok(())
}

fn write_empty_zip(path: &Path) -> std::io::Result<()> {
    let file = fs::File::create(path)?;
    zip::ZipWriter::new(file).finish().map_err(zip_io_error)?;
    Ok(())
}

#[cfg(windows)]
fn shell_shortcut(target: &Path, shortcut: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn quote(path: &Path) -> String {
        path.to_string_lossy().replace('\'', "''")
    }

    let workdir = target.parent().unwrap_or_else(|| Path::new(r"C:\"));
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.WorkingDirectory='{}';$s.Save()",
        quote(shortcut),
        quote(target),
        quote(workdir)
    );
    let status = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(tr("PowerShell не смог создать ярлык")))
    }
}

#[cfg(not(windows))]
fn shell_shortcut(_target: &Path, _shortcut: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        tr("Ярлыки поддерживаются только в Windows"),
    ))
}

const XLSX_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;
const XLSX_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
const XLSX_WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Лист1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
const XLSX_WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
const XLSX_SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#;
const XLSX_STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs></styleSheet>"#;

fn write_xlsx(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;

    let file = fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = || {
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
    };
    for (name, content) in [
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK),
        ("xl/_rels/workbook.xml.rels", XLSX_WORKBOOK_RELS),
        ("xl/worksheets/sheet1.xml", XLSX_SHEET),
        ("xl/styles.xml", XLSX_STYLES),
    ] {
        zip.start_file(name, options()).map_err(zip_io_error)?;
        zip.write_all(content.as_bytes())?;
    }
    zip.finish().map_err(zip_io_error)?;
    Ok(())
}

fn sort_fs_entries(entries: &mut [FsEntry], sort: SortState, folders_first: bool) {
    let compare = |a: &FsEntry, b: &FsEntry| {
        if folders_first {
            let directories = (!a.is_dir).cmp(&(!b.is_dir));
            if directories != std::cmp::Ordering::Equal {
                return directories;
            }
        }
        let order = match sort.key {
            SortKey::Name => a.name_lower.cmp(&b.name_lower),
            SortKey::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Kind => a
                .kind
                .cmp(b.kind)
                .then_with(|| a.name_lower.cmp(&b.name_lower)),
        };
        if sort.ascending {
            order
        } else {
            order.reverse()
        }
    };
    if entries.len() >= 50_000 {
        entries.par_sort_unstable_by(compare);
    } else {
        entries.sort_by(compare);
    }
}

fn read_directory(path: &Path, show_hidden: bool) -> std::io::Result<Vec<FsEntry>> {
    #[cfg(windows)]
    {
        if let Ok(entries) = read_directory_win32(path, show_hidden) {
            return Ok(entries);
        }
    }
    read_directory_std(path, show_hidden)
}

fn read_directory_std(path: &Path, show_hidden: bool) -> std::io::Result<Vec<FsEntry>> {
    let read_dir = fs::read_dir(path)?;
    let mut entries = Vec::new();
    for item in read_dir {
        let Ok(item) = item else { continue };
        let Ok(metadata) = item.metadata() else {
            continue;
        };
        #[cfg(windows)]
        let hidden = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & (0x2 | 0x4) != 0
        };
        #[cfg(not(windows))]
        let hidden = item.file_name().to_string_lossy().starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        let is_dir = metadata.is_dir();
        entries.push(make_entry(
            item.path(),
            is_dir,
            if is_dir { None } else { Some(metadata.len()) },
            metadata.modified().ok(),
        ));
    }
    Ok(entries)
}

#[cfg(windows)]
fn read_directory_win32(path: &Path, show_hidden: bool) -> std::io::Result<Vec<FsEntry>> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
            FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
        },
    };

    let mut pattern: Vec<u16> = crate::path_util::extended(&path.join("*"))
        .as_os_str()
        .encode_wide()
        .collect();
    pattern.push(0);
    let mut data = unsafe { std::mem::zeroed::<WIN32_FIND_DATAW>() };
    let handle = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            &mut data as *mut _ as *mut std::ffi::c_void,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }

    let mut entries = Vec::new();
    loop {
        let name_len = data
            .cFileName
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(data.cFileName.len());
        let name = std::ffi::OsString::from_wide(&data.cFileName[..name_len]);
        if name != "." && name != ".." {
            let hidden =
                data.dwFileAttributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0;
            if show_hidden || !hidden {
                let is_dir = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                let size = if is_dir {
                    None
                } else {
                    Some(((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64)
                };
                entries.push(make_entry(
                    path.join(Path::new(&name)),
                    is_dir,
                    size,
                    filetime_to_systemtime(data.ftLastWriteTime),
                ));
            }
        }

        if unsafe { FindNextFileW(handle, &mut data) } == 0 {
            let error = unsafe { GetLastError() };
            unsafe { FindClose(handle) };
            if error == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(std::io::Error::from_raw_os_error(error as i32));
        }
    }
    Ok(entries)
}

#[cfg(windows)]
fn filetime_to_systemtime(time: windows_sys::Win32::Foundation::FILETIME) -> Option<SystemTime> {
    const UNIX_EPOCH_100NS: u64 = 116_444_736_000_000_000;
    let ticks = ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
    if ticks < UNIX_EPOCH_100NS {
        return None;
    }
    let delta = ticks - UNIX_EPOCH_100NS;
    Some(UNIX_EPOCH + Duration::new(delta / 10_000_000, ((delta % 10_000_000) * 100) as u32))
}

fn make_entry(
    path: PathBuf,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<SystemTime>,
) -> FsEntry {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let name_lower = name.to_lowercase();
    FsEntry {
        name,
        name_lower,
        is_dir,
        size,
        modified,
        size_text: OnceLock::new(),
        modified_text: OnceLock::new(),
        kind: kind_label(&path, is_dir),
        color: category_color(&path, is_dir),
        path,
    }
}

fn volume_root(path: &Path) -> PathBuf {
    path.ancestors()
        .last()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf())
}

pub(crate) fn index_cache_path(root: &Path) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in root.display().to_string().to_lowercase().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Korobka")
        .join("index")
        .join(format!("{hash:016x}.idx"))
}

impl eframe::App for KorobkaApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        set_accent(self.accent);
        set_real_glass(self.appearance == Appearance::Glass);
        set_glass_transparency(self.glass_transparency);
        if !self.style_ready {
            configure_style(ctx);
            self.style_ready = true;
        }
        self.apply_visual_settings(ctx);
        if !self.backdrop_attempted {
            self.backdrop_ready = apply_windows_backdrop(
                self.appearance,
                self.glass_blur,
                self.glass_transparency,
            );
            self.backdrop_attempted = true;
        }
        paint_glass_background(ctx, self.appearance, self.backdrop_ready);
        self.poll_search(ctx);
        self.poll_dir_load(ctx);
        self.poll_ops(ctx);
        self.poll_locks(ctx);
        self.handle_dropped_files(ctx);
        self.handle_shortcuts(ctx);
        if self.filter_dirty {
            self.rebuild_filter();
        }

        self.draw_title_bar(ctx);
        self.draw_tab_bar(ctx);
        self.draw_nav_bar(ctx);

        let mut reset_index = false;
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(28.0)
            .frame(
                egui::Frame::NONE
                    .fill(glass_strong())
                    .stroke(egui::Stroke::new(1.0_f32, glass_border()))
                    .inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&self.status).size(10.5).color(
                        if self.status == tr("Готово") {
                            SUCCESS
                        } else {
                            TEXT_MUTED
                        },
                    ));
                    if !self.search_summary.is_empty() {
                        ui.separator();
                        ui.label(
                            egui::RichText::new(&self.search_summary)
                                .size(10.5)
                                .color(TEXT_FAINT),
                        );
                    }
                    ui.separator();
                    let count = if self.search_state.is_some() || !self.search_summary.is_empty() {
                        self.search_results.len()
                    } else {
                        self.entries.len()
                    };
                    ui.label(
                        egui::RichText::new(items_text(count))
                            .size(10.5)
                            .color(TEXT_FAINT),
                    );
                    if !self.selection.is_empty() {
                        ui.separator();
                        let selected_size: u64 = self
                            .entries
                            .iter()
                            .filter(|entry| self.selection.contains(&entry.path))
                            .filter_map(|entry| entry.size)
                            .sum();
                        let suffix = if selected_size > 0 {
                            format!(" · {}", format_size(selected_size))
                        } else {
                            String::new()
                        };
                        ui.label(
                            egui::RichText::new(if i18n::english() {
                                format!("Selected: {}{}", self.selection.len(), suffix)
                            } else {
                                format!("Выбрано: {}{}", self.selection.len(), suffix)
                            })
                            .size(10.5)
                            .color(accent_bright()),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(tr("Сброс"))
                            .on_hover_text(
                                tr("Удалить кэш индекса диска — следующий поиск построит его заново"),
                            )
                            .clicked()
                        {
                            reset_index = true;
                        }
                        if let Some(state) = &self.op_state {
                            if ui.small_button(tr("Отменить")).clicked() {
                                state.cancel.store(true, Ordering::Relaxed);
                            }
                        }
                        if !self.op_progress.is_empty() {
                            ui.label(
                                egui::RichText::new(&self.op_progress)
                                    .size(10.5)
                                    .color(TEXT_MUTED),
                            );
                        }
                        ui.label(
                            egui::RichText::new(match self.view {
                                ViewMode::List => tr("Список"),
                                ViewMode::Tiles => tr("Плитка"),
                            })
                            .size(10.5)
                            .color(TEXT_FAINT),
                        );
                    });
                });
            });
        if reset_index {
            self.invalidate_index();
            self.status = tr("Индекс сброшен").to_owned();
        }
        self.draw_sidebar(ctx);
        self.draw_properties_panel(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::TRANSPARENT))
            .show(ctx, |ui| self.draw_central(ui));
        self.draw_settings(ctx);
        self.flush_settings_save(ctx);
        self.draw_picker(ctx);
        self.draw_trash(ctx);
        self.draw_trash_confirm(ctx);
        self.draw_rename(ctx);
        self.draw_confirm_permanent_delete(ctx);
        self.draw_lock_dialog(ctx);
        self.draw_op_conflict(ctx);
        draw_resize_handles(ctx);
        draw_drop_overlay(ctx);
    }
}

fn draw_drop_overlay(ctx: &egui::Context) {
    if ctx.input(|input| input.raw.hovered_files.is_empty()) {
        return;
    }
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop_overlay"),
    ));
    painter.rect_filled(
        rect,
        0.0,
        egui::Color32::from_rgba_unmultiplied(5, 10, 20, 220),
    );
    painter.rect_stroke(
        rect.shrink(10.0),
        12.0,
        egui::Stroke::new(2.0_f32, accent_bright()),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if i18n::english() {
            "Release to copy\nShift — move"
        } else {
            "Отпустите, чтобы скопировать\nShift — переместить"
        },
        egui::FontId::proportional(18.0),
        TEXT,
    );
}

#[derive(Default)]
struct RowMenu {
    open: bool,
    open_with: bool,
    open_new_tab: bool,
    copy: bool,
    cut: bool,
    copy_paths: bool,
    rename: bool,
    properties: bool,
    trash: bool,
    permanent: bool,
    create: Option<(CreateKind, PathBuf)>,
    shortcut: Option<PathBuf>,
}

fn create_item(
    ui: &mut egui::Ui,
    label: &str,
    kind: CreateKind,
    target: &Path,
    create: &mut Option<(CreateKind, PathBuf)>,
) {
    if ui.button(label).clicked() {
        *create = Some((kind, target.to_path_buf()));
        ui.close_menu();
    }
}

fn create_submenu(
    ui: &mut egui::Ui,
    target: &Path,
    create: &mut Option<(CreateKind, PathBuf)>,
) {
    ui.menu_button(tr("Создать"), |ui| {
        create_item(ui, tr("Папку"), CreateKind::Folder, target, create);
        create_item(ui, tr("Текстовый документ"), CreateKind::Text, target, create);
        create_item(ui, tr("Документ Word"), CreateKind::Word, target, create);
        create_item(ui, tr("Таблица Excel"), CreateKind::Excel, target, create);
        create_item(ui, tr("ZIP-архив"), CreateKind::Zip, target, create);
        create_item(ui, tr("Пустой файл"), CreateKind::EmptyFile, target, create);
    });
}

fn row_context_menu(
    response: &egui::Response,
    is_dir: bool,
    item: &Path,
    dir: Option<&Path>,
) -> RowMenu {
    let mut menu = RowMenu::default();
    let mut create = None;
    response.context_menu(|ui| {
        if ui.button(tr("Открыть")).clicked() {
            menu.open = true;
            ui.close_menu();
        }
        if !is_dir && ui.button(tr("Открыть с помощью…")).clicked() {
            menu.open_with = true;
            ui.close_menu();
        }
        if is_dir && ui.button(tr("Открыть в новой вкладке")).clicked() {
            menu.open_new_tab = true;
            ui.close_menu();
        }
        if let Some(dir) = dir {
            ui.separator();
            create_submenu(ui, dir, &mut create);
        }
        ui.separator();
        if ui.button(tr("Создать ярлык")).clicked() {
            menu.shortcut = Some(item.to_path_buf());
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Копировать")).clicked() {
            menu.copy = true;
            ui.close_menu();
        }
        if ui.button(tr("Вырезать")).clicked() {
            menu.cut = true;
            ui.close_menu();
        }
        if ui.button(tr("Копировать путь")).clicked() {
            menu.copy_paths = true;
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Переименовать")).clicked() {
            menu.rename = true;
            ui.close_menu();
        }
        if ui.button(tr("Свойства")).clicked() {
            menu.properties = true;
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Удалить в корзину")).clicked() {
            menu.trash = true;
            ui.close_menu();
        }
        if ui.button(tr("Удалить безвозвратно")).clicked() {
            menu.permanent = true;
            ui.close_menu();
        }
    });
    menu.create = create;
    menu
}

fn release_text_focus(ctx: &egui::Context) {
    if let Some(focused) = ctx.memory(|memory| memory.focused()) {
        ctx.memory_mut(|memory| memory.surrender_focus(focused));
    }
}

fn apply_selection<T: PathItem>(
    selection: &mut BTreeSet<PathBuf>,
    anchor: &mut Option<PathBuf>,
    selected: &mut Option<PathBuf>,
    items: &[T],
    index: usize,
    modifiers: egui::Modifiers,
) {
    let Some(path) = items.get(index).map(|item| item.path().to_path_buf()) else {
        return;
    };
    if modifiers.shift {
        if let Some(anchor_path) = anchor {
            if let Some(anchor_index) = items.iter().position(|item| item.path() == anchor_path) {
                let (start, end) = if anchor_index <= index {
                    (anchor_index, index)
                } else {
                    (index, anchor_index)
                };
                if !modifiers.command {
                    selection.clear();
                }
                for item in &items[start..=end] {
                    selection.insert(item.path().to_path_buf());
                }
                return;
            }
        }
        *anchor = Some(path.clone());
        selection.clear();
        selection.insert(path.clone());
    } else if modifiers.command {
        if !selection.remove(&path) {
            selection.insert(path.clone());
        }
        *anchor = Some(path.clone());
    } else {
        selection.clear();
        selection.insert(path.clone());
        *anchor = Some(path.clone());
    }
    *selected = Some(path);
}

fn apply_selection_visible<T: PathItem>(
    selection: &mut BTreeSet<PathBuf>,
    anchor: &mut Option<PathBuf>,
    selected: &mut Option<PathBuf>,
    items: &[T],
    visible: &[usize],
    row: usize,
    modifiers: egui::Modifiers,
) {
    let Some(&index) = visible.get(row) else {
        return;
    };
    let Some(path) = items.get(index).map(|item| item.path().to_path_buf()) else {
        return;
    };
    if modifiers.shift {
        if let Some(anchor_path) = anchor {
            if let Some(anchor_row) = visible.iter().position(|&item_index| {
                items
                    .get(item_index)
                    .is_some_and(|item| item.path() == anchor_path)
            }) {
                let (start, end) = if anchor_row <= row {
                    (anchor_row, row)
                } else {
                    (row, anchor_row)
                };
                if !modifiers.command {
                    selection.clear();
                }
                for &item_index in &visible[start..=end] {
                    if let Some(item) = items.get(item_index) {
                        selection.insert(item.path().to_path_buf());
                    }
                }
                *selected = Some(path);
                return;
            }
        }
        *anchor = Some(path.clone());
        selection.clear();
        selection.insert(path.clone());
    } else if modifiers.command {
        if !selection.remove(&path) {
            selection.insert(path.clone());
        }
        *anchor = Some(path.clone());
    } else {
        selection.clear();
        selection.insert(path.clone());
        *anchor = Some(path.clone());
    }
    *selected = Some(path);
}

fn paint_glow(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    [red, green, blue]: [u8; 3],
    alpha: u8,
) {
    let segments = 40;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(
        center,
        egui::Color32::from_rgba_unmultiplied(red, green, blue, alpha),
    );
    for index in 0..=segments {
        let angle = index as f32 / segments as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(
            center + egui::vec2(angle.cos(), angle.sin()) * radius,
            egui::Color32::from_rgba_unmultiplied(red, green, blue, 0),
        );
    }
    for index in 0..segments {
        mesh.add_triangle(0, index + 1, index + 2);
    }
    painter.add(mesh);
}

fn paint_glass_background(ctx: &egui::Context, appearance: Appearance, acrylic: bool) {
    let rect = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let glass = appearance == Appearance::Glass;

    let alpha = if glass {
        // Настоящее стекло: слой поверх акрила (или без блюра — чуть плотнее).
        let base = glass_bg_alpha();
        if acrylic {
            base
        } else {
            (base + 25).min(120)
        }
    } else if acrylic {
        236
    } else {
        255
    };
    let top = egui::Color32::from_rgba_unmultiplied(11, 18, 35, alpha);
    let bottom = egui::Color32::from_rgba_unmultiplied(7, 11, 22, alpha);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);

    let shortest = rect.width().min(rect.height());
    let [accent_r, accent_g, accent_b] = accent_rgb();
    let glow_alpha: u8 = if glass { 12 } else { 18 };
    let blobs = [
        (
            egui::pos2(
                rect.left() + rect.width() * 0.16,
                rect.top() - rect.height() * 0.04,
            ),
            shortest * 0.52,
            [accent_r, accent_g, accent_b],
            glow_alpha,
        ),
        (
            egui::pos2(
                rect.left() + rect.width() * 0.9,
                rect.top() + rect.height() * 0.2,
            ),
            shortest * 0.44,
            [146, 92, 255],
            glow_alpha.saturating_sub(3),
        ),
        (
            egui::pos2(
                rect.left() + rect.width() * 0.5,
                rect.bottom() + rect.height() * 0.1,
            ),
            shortest * 0.48,
            [38, 180, 188],
            glow_alpha.saturating_sub(5),
        ),
    ];
    for (center, radius, color, alpha) in blobs {
        paint_glow(&painter, center, radius, color, alpha);
        paint_glow(&painter, center, radius * 0.52, color, alpha / 2);
    }

    painter.rect_filled(
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.top() + 1.0)),
        0.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, if glass { 14 } else { 18 }),
    );
}

#[cfg(windows)]
#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[cfg(windows)]
#[repr(C)]
struct WindowCompositionAttributeData {
    attribute: u32,
    data: *mut std::ffi::c_void,
    size: usize,
}

#[cfg(windows)]
const WCA_ACCENT_POLICY: u32 = 19;

#[cfg(windows)]
fn acrylic_tint(appearance: Appearance, transparency: u8) -> u8 {
    match appearance {
        Appearance::Matte => 0xCC,
        Appearance::Glass => {
            let transparency = transparency.min(100) as u32;
            (0x78 - transparency * 0x48 / 100) as u8
        }
    }
}

#[cfg(windows)]
fn apply_windows_backdrop(appearance: Appearance, blur: bool, transparency: u8) -> bool {
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
    let want_blur = appearance == Appearance::Glass && blur;
    let state = if want_blur {
        ACCENT_ENABLE_ACRYLICBLURBEHIND
    } else {
        0
    };
    set_windows_accent_policy(state, acrylic_tint(appearance, transparency)) && want_blur
}

#[cfg(windows)]
fn set_windows_accent_policy(state: u32, tint_alpha: u8) -> bool {
    use std::ffi::c_void;

    let mut policy = AccentPolicy {
        state,
        flags: 0,
        gradient_color: ((tint_alpha as u32) << 24) | 0x001A_2438,
        animation_id: 0,
    };
    let mut data = WindowCompositionAttributeData {
        attribute: WCA_ACCENT_POLICY,
        data: &mut policy as *mut AccentPolicy as *mut c_void,
        size: std::mem::size_of::<AccentPolicy>(),
    };

    let title: Vec<u16> = "Korobka Explorer\0".encode_utf16().collect();
    let hwnd = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(std::ptr::null(), title.as_ptr())
    };
    if hwnd.is_null() {
        return false;
    }

    let module_name: Vec<u16> = "user32.dll\0".encode_utf16().collect();
    let module = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(module_name.as_ptr())
    };
    if module.is_null() {
        return false;
    }
    let proc_name = b"SetWindowCompositionAttribute\0";
    let proc = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetProcAddress(module, proc_name.as_ptr())
    };
    let Some(proc) = proc else {
        return false;
    };
    type SetWindowCompositionAttributeFn =
        unsafe extern "system" fn(*mut c_void, *mut WindowCompositionAttributeData) -> i32;
    let set_window_composition: SetWindowCompositionAttributeFn =
        unsafe { std::mem::transmute(proc) };
    apply_dark_window_chrome(hwnd);
    unsafe { set_window_composition(hwnd, &mut data) != 0 }
}

#[cfg(windows)]
fn apply_dark_window_chrome(hwnd: *mut std::ffi::c_void) {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut std::ffi::c_void,
            attribute: u32,
            value: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_TEXT_COLOR: u32 = 36;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;

    let dark: i32 = 1;
    let caption: u32 = 0x0018100C;
    let text: u32 = 0x00F5EDE6;
    let border: u32 = 0x00261C12;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &caption as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            &text as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border as *const u32 as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn apply_windows_backdrop(_appearance: Appearance, _blur: bool, _transparency: u8) -> bool {
    false
}

fn apply_visual_colors(ctx: &egui::Context) {
    let mut visuals = ctx.style().visuals.clone();
    visuals.window_fill = glass_strong();
    visuals.extreme_bg_color = glass_input_bg();
    visuals.selection.bg_fill = glass_accent();
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, accent_bright());
    visuals.widgets.active.bg_fill = glass_accent();
    visuals.widgets.active.weak_bg_fill = glass_accent();
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, accent_dim());
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, glass_border());
    visuals.widgets.open.bg_fill = glass_strong();
    visuals.widgets.open.weak_bg_fill = glass_strong();
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, glass_border());
    visuals.window_stroke = egui::Stroke::new(1.0_f32, glass_border());
    ctx.set_visuals(visuals);
}

fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::TRANSPARENT;
    visuals.window_fill = glass_strong();
    visuals.extreme_bg_color = glass_input_bg();
    visuals.faint_bg_color = TABLE_STRIPE;
    visuals.selection.bg_fill = glass_accent();
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, accent_bright());
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8);
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, glass_border());
    visuals.widgets.active.bg_fill = glass_accent();
    visuals.widgets.active.weak_bg_fill = glass_accent();
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, accent_dim());
    visuals.widgets.open.bg_fill = glass_strong();
    visuals.widgets.open.weak_bg_fill = glass_strong();
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, glass_border());
    visuals.window_stroke = egui::Stroke::new(1.0_f32, glass_border());
    visuals.window_corner_radius = egui::CornerRadius::same(14);
    visuals.menu_corner_radius = egui::CornerRadius::same(12);
    ctx.set_visuals(visuals);

    if let Ok(data) = fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "segoe-ui".to_owned(),
            egui::FontData::from_owned(data).into(),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe-ui".to_owned());
        ctx.set_fonts(fonts);
    }

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.menu_margin = egui::Margin::same(6);
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(16.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(12.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(10.5, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(11.5, egui::FontFamily::Monospace),
    );
    ctx.set_style(style);
}

fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: Icon, color: egui::Color32) {
    let r = rect.shrink(rect.width() * 0.12);
    let s = r.width();
    let stroke = egui::Stroke::new((s * 0.09).clamp(1.1, 1.8), color);
    let p = |x: f32, y: f32| egui::pos2(r.left() + x * s, r.top() + y * s);
    let line = |points: Vec<egui::Pos2>| {
        painter.add(egui::Shape::line(points, stroke));
    };
    let poly = |points: Vec<egui::Pos2>| {
        painter.add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    };
    match icon {
        Icon::Back => line(vec![p(0.62, 0.18), p(0.3, 0.5), p(0.62, 0.82)]),
        Icon::Forward => line(vec![p(0.38, 0.18), p(0.7, 0.5), p(0.38, 0.82)]),
        Icon::Up => {
            line(vec![p(0.5, 0.82), p(0.5, 0.2)]);
            line(vec![p(0.22, 0.46), p(0.5, 0.2), p(0.78, 0.46)]);
        }
        Icon::Refresh => {
            painter.circle_stroke(p(0.5, 0.52), s * 0.28, stroke);
            line(vec![p(0.62, 0.13), p(0.78, 0.25), p(0.64, 0.37)]);
            painter.rect_filled(
                egui::Rect::from_min_max(p(0.56, 0.05), p(0.86, 0.34)),
                0.0,
                BG,
            );
            line(vec![p(0.62, 0.2), p(0.79, 0.27), p(0.68, 0.42)]);
        }
        Icon::Search => {
            painter.circle_stroke(p(0.43, 0.42), s * 0.24, stroke);
            line(vec![p(0.62, 0.62), p(0.84, 0.84)]);
        }
        Icon::List => {
            for y in [0.25, 0.5, 0.75] {
                painter.circle_filled(p(0.16, y), s * 0.045, color);
                line(vec![p(0.32, y), p(0.86, y)]);
            }
        }
        Icon::Grid => {
            for (x, y) in [(0.16, 0.16), (0.55, 0.16), (0.16, 0.55), (0.55, 0.55)] {
                painter.rect_stroke(
                    egui::Rect::from_min_size(p(x, y), egui::vec2(s * 0.28, s * 0.28)),
                    2.0,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        Icon::Sidebar => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.1, 0.16), p(0.9, 0.84)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(vec![p(0.4, 0.18), p(0.4, 0.82)]);
        }
        Icon::Plus => {
            line(vec![p(0.5, 0.16), p(0.5, 0.84)]);
            line(vec![p(0.16, 0.5), p(0.84, 0.5)]);
        }
        Icon::Close => {
            line(vec![p(0.22, 0.22), p(0.78, 0.78)]);
            line(vec![p(0.78, 0.22), p(0.22, 0.78)]);
        }
        Icon::Folder => {
            line(vec![
                p(0.1, 0.28),
                p(0.42, 0.28),
                p(0.5, 0.18),
                p(0.9, 0.18),
                p(0.9, 0.82),
                p(0.1, 0.82),
                p(0.1, 0.28),
            ]);
        }
        Icon::File => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.2, 0.1), p(0.8, 0.9)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(vec![p(0.58, 0.12), p(0.8, 0.34)]);
            line(vec![p(0.58, 0.12), p(0.58, 0.34), p(0.8, 0.34)]);
            line(vec![p(0.34, 0.56), p(0.66, 0.56)]);
            line(vec![p(0.34, 0.7), p(0.6, 0.7)]);
        }
        Icon::Drive => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.1, 0.32), p(0.9, 0.76)),
                4.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(vec![p(0.1, 0.58), p(0.9, 0.58)]);
            painter.circle_filled(p(0.76, 0.68), s * 0.04, color);
        }
        Icon::More => {
            for x in [0.22, 0.5, 0.78] {
                painter.circle_filled(p(x, 0.5), s * 0.065, color);
            }
        }
        Icon::Properties => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.18, 0.12), p(0.82, 0.88)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            for y in [0.34, 0.5, 0.66] {
                line(vec![p(0.32, y), p(0.68, y)]);
            }
        }
        Icon::Copy => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.3, 0.3), p(0.86, 0.86)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(vec![p(0.14, 0.66), p(0.14, 0.14), p(0.66, 0.14)]);
        }
        Icon::Cut => {
            line(vec![p(0.22, 0.18), p(0.72, 0.68)]);
            line(vec![p(0.78, 0.18), p(0.28, 0.68)]);
            painter.circle_stroke(p(0.2, 0.78), s * 0.1, stroke);
            painter.circle_stroke(p(0.8, 0.78), s * 0.1, stroke);
        }
        Icon::Paste => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.18, 0.2), p(0.82, 0.88)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.36, 0.08), p(0.64, 0.28)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        Icon::Rename | Icon::Edit => {
            line(vec![
                p(0.2, 0.8),
                p(0.3, 0.55),
                p(0.72, 0.13),
                p(0.87, 0.28),
                p(0.45, 0.7),
                p(0.2, 0.8),
            ]);
            line(vec![p(0.3, 0.55), p(0.45, 0.7)]);
        }
        Icon::Trash => {
            line(vec![p(0.2, 0.26), p(0.8, 0.26)]);
            line(vec![
                p(0.38, 0.26),
                p(0.38, 0.14),
                p(0.62, 0.14),
                p(0.62, 0.26),
            ]);
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.26, 0.3), p(0.74, 0.86)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        Icon::Undo => {
            line(vec![p(0.14, 0.5), p(0.34, 0.3), p(0.34, 0.7), p(0.14, 0.5)]);
            line(vec![p(0.34, 0.5), p(0.62, 0.5)]);
            painter.circle_stroke(p(0.62, 0.5), s * 0.24, stroke);
        }
        Icon::ChevronDown => line(vec![p(0.24, 0.38), p(0.5, 0.64), p(0.76, 0.38)]),
        Icon::ChevronRight => line(vec![p(0.38, 0.22), p(0.64, 0.5), p(0.38, 0.78)]),
        Icon::Computer | Icon::Desktop => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.12, 0.16), p(0.88, 0.68)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(vec![p(0.5, 0.68), p(0.5, 0.82)]);
            line(vec![p(0.3, 0.82), p(0.7, 0.82)]);
        }
        Icon::Download => {
            line(vec![p(0.5, 0.14), p(0.5, 0.66)]);
            line(vec![p(0.28, 0.46), p(0.5, 0.68), p(0.72, 0.46)]);
            line(vec![
                p(0.18, 0.76),
                p(0.18, 0.88),
                p(0.82, 0.88),
                p(0.82, 0.76),
            ]);
        }
        Icon::Image => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.12, 0.18), p(0.88, 0.82)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_filled(p(0.34, 0.36), s * 0.06, color);
            line(vec![
                p(0.16, 0.72),
                p(0.4, 0.5),
                p(0.56, 0.66),
                p(0.68, 0.54),
                p(0.86, 0.72),
            ]);
        }
        Icon::Music => {
            line(vec![p(0.42, 0.2), p(0.42, 0.7)]);
            line(vec![p(0.42, 0.2), p(0.78, 0.12), p(0.78, 0.62)]);
            painter.circle_filled(p(0.34, 0.72), s * 0.1, color);
            painter.circle_filled(p(0.7, 0.64), s * 0.1, color);
        }
        Icon::Video => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(0.1, 0.2), p(0.9, 0.8)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            poly(vec![p(0.42, 0.34), p(0.68, 0.5), p(0.42, 0.66)]);
        }
    }
}

fn draw_resize_handles(ctx: &egui::Context) {
    if ctx.input(|input| input.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let screen = ctx.screen_rect();
    let edge = 6.0;
    let corner = 16.0;
    let handles = [
        (
            egui::Rect::from_min_max(
                screen.left_top(),
                egui::pos2(screen.left() + corner, screen.top() + corner),
            ),
            egui::ResizeDirection::NorthWest,
            egui::CursorIcon::ResizeNwSe,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(screen.right() - corner, screen.top()),
                egui::pos2(screen.right(), screen.top() + corner),
            ),
            egui::ResizeDirection::NorthEast,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(screen.left(), screen.bottom() - corner),
                egui::pos2(screen.left() + corner, screen.bottom()),
            ),
            egui::ResizeDirection::SouthWest,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(screen.right() - corner, screen.bottom() - corner),
                screen.right_bottom(),
            ),
            egui::ResizeDirection::SouthEast,
            egui::CursorIcon::ResizeNwSe,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(screen.left() + corner, screen.top()),
                egui::pos2(screen.right() - corner, screen.top() + edge),
            ),
            egui::ResizeDirection::North,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(screen.left() + corner, screen.bottom() - edge),
                egui::pos2(screen.right() - corner, screen.bottom()),
            ),
            egui::ResizeDirection::South,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            egui::Rect::from_min_max(
                screen.left_top() + egui::vec2(0.0, corner),
                egui::pos2(screen.left() + edge, screen.bottom() - corner),
            ),
            egui::ResizeDirection::West,
            egui::CursorIcon::ResizeHorizontal,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(screen.right() - edge, screen.top() + corner),
                egui::pos2(screen.right(), screen.bottom() - corner),
            ),
            egui::ResizeDirection::East,
            egui::CursorIcon::ResizeHorizontal,
        ),
    ];

    for (index, (rect, direction, cursor)) in handles.into_iter().enumerate() {
        egui::Area::new(egui::Id::new(("resize_handle", index)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                ui.set_min_size(rect.size());
                let local = egui::Rect::from_min_size(egui::Pos2::ZERO, rect.size());
                let response = ui
                    .interact(
                        local,
                        ui.id().with(("resize_edge", index)),
                        egui::Sense::drag(),
                    )
                    .on_hover_cursor(cursor);
                if response.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
                }
            });
    }
}

fn window_control(ui: &mut egui::Ui, kind: u8) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(38.0, 24.0), egui::Sense::click());
    if response.hovered() {
        let fill = if kind == 2 {
            egui::Color32::from_rgb(0xC4, 0x2B, 0x1C)
        } else {
            GLASS_HIGHLIGHT
        };
        ui.painter().rect_filled(rect, 6.0, fill);
    }
    let color = if response.hovered() { TEXT } else { TEXT_MUTED };
    let stroke = egui::Stroke::new(1.2_f32, color);
    let center = rect.center();
    match kind {
        0 => {
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 5.0, center.y),
                    egui::pos2(center.x + 5.0, center.y),
                ],
                stroke,
            );
        }
        1 => {
            ui.painter().rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(9.0, 9.0)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        _ => {
            ui.painter().line_segment(
                [
                    egui::pos2(center.x - 4.5, center.y - 4.5),
                    egui::pos2(center.x + 4.5, center.y + 4.5),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 4.5, center.y - 4.5),
                    egui::pos2(center.x - 4.5, center.y + 4.5),
                ],
                stroke,
            );
        }
    }
    response
}

fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    tooltip: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), sense);
    if selected {
        ui.painter().rect_filled(rect, 6.0, SELECTED);
    } else if enabled && response.hovered() {
        ui.painter().rect_filled(rect, 8.0, glass_soft());
    }
    let color = if !enabled {
        TEXT_DISABLED
    } else if selected {
        accent()
    } else if response.hovered() {
        TEXT
    } else {
        TEXT_MUTED
    };
    paint_icon(ui.painter(), rect.shrink(6.0), icon, color);
    response.on_hover_text(tooltip)
}

fn nav_item(ui: &mut egui::Ui, icon: Icon, label: &str, selected: bool, collapsed: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 6.0, SELECTED);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height())),
            1.0,
            accent(),
        );
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 8.0, glass_soft());
    }
    let color = if selected {
        accent()
    } else if response.hovered() {
        TEXT
    } else {
        TEXT_MUTED
    };
    if collapsed {
        paint_icon(
            ui.painter(),
            rect.shrink2(egui::vec2(11.0, 5.0)),
            icon,
            color,
        );
    } else {
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 17.0, rect.center().y),
            egui::vec2(17.0, 17.0),
        );
        paint_icon(ui.painter(), icon_rect, icon, color);
        let painter = ui
            .painter()
            .with_clip_rect(rect.shrink2(egui::vec2(4.0, 0.0)));
        painter.text(
            egui::pos2(rect.left() + 34.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            color,
        );
    }
    response.on_hover_text(label).clicked()
}

fn session_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("Korobka").join("session.json"))
}

fn load_session() -> Option<SessionState> {
    let path = session_path()?;
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_session(app: &KorobkaApp) {
    let Some(path) = session_path() else {
        return;
    };
    let Ok(data) = serde_json::to_string(&session_state(app)) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temp = path.with_file_name("session.json.tmp");
    if fs::write(&temp, data).is_ok() && fs::rename(&temp, &path).is_err() {
        let _ = fs::remove_file(&temp);
    }
}

fn session_state(app: &KorobkaApp) -> SessionState {
    SessionState {
        tabs: app.tabs.iter().map(|tab| tab.dir.clone()).collect(),
        active_tab: app.active_tab,
        view: if app.view == ViewMode::Tiles { 1 } else { 0 },
        sort_key: match app.sort.key {
            SortKey::Name => 0,
            SortKey::Size => 1,
            SortKey::Modified => 2,
            SortKey::Kind => 3,
        },
        sort_ascending: app.sort.ascending,
        sidebar_width: app.sidebar_width,
        sidebar_collapsed: app.sidebar_collapsed,
        compact_rows: app.compact_rows,
        appearance: if app.appearance == Appearance::Glass { 1 } else { 0 },
        accent: app.accent,
        language: match app.language {
            Language::Russian => 1,
            Language::English => 2,
            Language::System => 0,
        },
        glass_blur: app.glass_blur,
        glass_transparency: app.glass_transparency,
        folders_first: app.folders_first,
        show_hidden: app.show_hidden,
        striped_rows: app.striped_rows,
        ui_scale: app.ui_scale,
        animations: app.animations,
        confirm_permanent_delete: app.confirm_permanent_delete,
        search_mode_index: app.search_mode_index,
        search_threads: app.search_threads,
    }
}

fn normalize_archive_inner(inner: &str) -> String {
    let inner = inner.replace('\\', "/");
    let inner = inner.trim_matches('/');
    if inner.is_empty() {
        String::new()
    } else {
        format!("{inner}/")
    }
}

fn tab_title(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            let text = path.display().to_string();
            if text.is_empty() {
                tr("Компьютер").to_owned()
            } else {
                text
            }
        })
}

fn place_icon(path: &Path) -> Icon {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "desktop" => Icon::Desktop,
        "downloads" => Icon::Download,
        "pictures" => Icon::Image,
        "music" => Icon::Music,
        "videos" => Icon::Video,
        _ => Icon::Folder,
    }
}

fn breadcrumbs(ui: &mut egui::Ui, path: &Path) -> Option<PathBuf> {
    let mut target = None;
    egui::ScrollArea::horizontal()
        .id_salt("breadcrumbs")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut chain: Vec<&Path> = path.ancestors().collect();
                chain.reverse();
                for (index, ancestor) in chain.into_iter().enumerate() {
                    if index > 0 {
                        ui.label(egui::RichText::new("›").size(12.0).color(TEXT_DISABLED));
                    }
                    let label = ancestor
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| ancestor.display().to_string());
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(label).size(12.0).color(TEXT_MUTED),
                            )
                            .frame(false)
                            .min_size(egui::vec2(0.0, 24.0)),
                        )
                        .clicked()
                    {
                        target = Some(ancestor.to_path_buf());
                    }
                }
            });
        });
    target
}

fn property_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [76.0, 18.0],
            egui::Label::new(egui::RichText::new(label).size(10.5).color(TEXT_FAINT))
                .wrap_mode(egui::TextWrapMode::Truncate),
        );
        ui.label(egui::RichText::new(value).size(11.5).color(TEXT_MUTED));
    });
}

fn sort_header(ui: &mut egui::Ui, label: &str, key: SortKey, sort: &mut SortState) -> bool {
    let arrow = if sort.key == key {
        if sort.ascending {
            " ^"
        } else {
            " v"
        }
    } else {
        ""
    };
    let text = format!("{label}{arrow}");
    let button = egui::Button::new(
        egui::RichText::new(text)
            .size(10.5)
            .strong()
            .color(TEXT_MUTED),
    )
    .frame(false);
    if ui.add(button).clicked() {
        if sort.key == key {
            sort.ascending = !sort.ascending;
        } else {
            sort.key = key;
            sort.ascending = true;
        }
        true
    } else {
        false
    }
}

fn name_cell(ui: &mut egui::Ui, is_dir: bool, color: egui::Color32, name: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        paint_icon(
            ui.painter(),
            rect,
            if is_dir { Icon::Folder } else { Icon::File },
            if is_dir { WARNING } else { color },
        );
        ui.add(
            egui::Label::new(egui::RichText::new(name).size(12.0).color(TEXT))
                .wrap_mode(egui::TextWrapMode::Truncate),
        );
    });
}

fn header_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(10.5)
            .strong()
            .color(TEXT_MUTED),
    );
}

fn sort_menu(ui: &mut egui::Ui, sort: &mut SortState) -> bool {
    let mut changed = false;
    for (label, key) in [
        (tr("Имя"), SortKey::Name),
        (tr("Размер"), SortKey::Size),
        (tr("Тип"), SortKey::Kind),
        (tr("Изменён"), SortKey::Modified),
    ] {
        if ui.selectable_label(sort.key == key, label).clicked() {
            sort.key = key;
            sort.ascending = true;
            changed = true;
            ui.close_menu();
        }
    }
    ui.separator();
    let direction = if sort.ascending {
        tr("По убыванию")
    } else {
        tr("По возрастанию")
    };
    if ui.button(direction).clicked() {
        sort.ascending = !sort.ascending;
        changed = true;
        ui.close_menu();
    }
    changed
}

fn empty_state(ui: &mut egui::Ui, icon: Icon, title: &str, subtitle: &str) {
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
        paint_icon(ui.painter(), rect.shrink(6.0), icon, TEXT_DISABLED);
        ui.add_space(4.0);
        ui.label(egui::RichText::new(title).size(12.5).color(TEXT_MUTED));
        ui.label(egui::RichText::new(subtitle).size(10.5).color(TEXT_FAINT));
    });
}

fn paint_tile(
    ui: &mut egui::Ui,
    entry: &FsEntry,
    selected: bool,
    width: f32,
    height: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 8.0, SELECTED);
        ui.painter().rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0_f32, accent_dim()),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 8.0, glass_soft());
    }
    let icon_size = (height * 0.34).clamp(28.0, 34.0);
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + height * 0.27),
        egui::vec2(icon_size, icon_size),
    );
    paint_icon(
        ui.painter(),
        icon_rect,
        if entry.is_dir {
            Icon::Folder
        } else {
            Icon::File
        },
        if entry.is_dir { WARNING } else { entry.color },
    );
    let name_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 8.0, rect.top() + height * 0.48),
        egui::pos2(rect.right() - 8.0, rect.bottom() - 16.0),
    );
    let painter = ui.painter().with_clip_rect(name_rect);
    let name_color = if selected { TEXT } else { TEXT_MUTED };
    let galley = painter.layout(
        entry.name.clone(),
        egui::FontId::proportional(11.5),
        name_color,
        name_rect.width(),
    );
    painter.galley(
        egui::pos2(
            name_rect.center().x - galley.size().x * 0.5,
            name_rect.top(),
        ),
        galley,
        name_color,
    );
    let meta = if entry.is_dir {
        tr("Папка").to_owned()
    } else {
        entry.size_text().to_owned()
    };
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 10.0),
        egui::Align2::CENTER_CENTER,
        meta,
        egui::FontId::proportional(9.5),
        TEXT_FAINT,
    );
    response.on_hover_text(entry.path.display().to_string())
}

fn category_color(path: &Path, is_dir: bool) -> egui::Color32 {
    if is_dir {
        return egui::Color32::from_rgb(214, 178, 94);
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff" => {
            egui::Color32::from_rgb(122, 194, 140)
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => egui::Color32::from_rgb(139, 162, 224),
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" => egui::Color32::from_rgb(190, 136, 196),
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => {
            egui::Color32::from_rgb(196, 162, 112)
        }
        "exe" | "msi" | "bat" | "cmd" | "ps1" => egui::Color32::from_rgb(178, 184, 194),
        "pdf" => egui::Color32::from_rgb(206, 122, 122),
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "cc" | "h" | "hpp" | "cs"
        | "java" | "go" | "rb" | "php" | "json" | "toml" | "yaml" | "yml" | "xml" | "html"
        | "css" => egui::Color32::from_rgb(124, 188, 206),
        "txt" | "md" | "log" | "ini" | "cfg" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => {
            egui::Color32::from_rgb(158, 184, 214)
        }
        _ => egui::Color32::from_rgb(142, 148, 158),
    }
}

fn kind_label(path: &Path, is_dir: bool) -> &'static str {
    if is_dir {
        return tr("Папка");
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff" => {
            tr("Изображение")
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => tr("Аудио"),
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" => tr("Видео"),
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => tr("Архив"),
        "exe" | "msi" | "bat" | "cmd" | "ps1" => tr("Программа"),
        "pdf" => "PDF",
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "cc" | "h" | "hpp" | "cs"
        | "java" | "go" | "rb" | "php" | "json" | "toml" | "yaml" | "yml" | "xml" | "html"
        | "css" => tr("Код"),
        "txt" | "md" | "log" | "ini" | "cfg" => tr("Текст"),
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => tr("Документ"),
        _ => tr("Файл"),
    }
}

#[cfg(windows)]
fn shell_execute(path: &Path, operation: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let operation: Vec<u16> = operation.encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        Err(tr_msg(
        format!("код {}", result as isize),
        format!("code {}", result as isize),
    ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn open_with_dialog(path: &Path) -> Result<(), String> {
    shell_execute(path, "openas")
}

#[cfg(windows)]
fn show_shell_properties(path: &Path) -> Result<(), String> {
    shell_execute(path, "properties")
}

#[cfg(not(windows))]
fn open_with_dialog(path: &Path) -> Result<(), String> {
    open::that(path).map_err(|err| err.to_string())
}

#[cfg(not(windows))]
fn show_shell_properties(path: &Path) -> Result<(), String> {
    open::that(path).map_err(|err| err.to_string())
}

#[cfg(windows)]
fn format_modified(time: Option<SystemTime>) -> String {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    let Some(time) = time else {
        return String::new();
    };
    let Ok(duration) = time.duration_since(UNIX_EPOCH) else {
        return String::new();
    };
    let ticks = 116_444_736_000_000_000_u64
        + duration.as_secs() * 10_000_000
        + u64::from(duration.subsec_nanos()) / 100;
    let filetime = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&filetime, &mut utc) } == 0 {
        return String::new();
    }
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return String::new();
    }
    format!(
        "{:02}.{:02}.{:04} {:02}:{:02}",
        local.wDay, local.wMonth, local.wYear, local.wHour, local.wMinute
    )
}

#[cfg(not(windows))]
fn format_modified(_time: Option<SystemTime>) -> String {
    String::new()
}

fn quick_places() -> Vec<(String, PathBuf)> {
    let mut places = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for (label, name) in [
            ("Домашняя папка", ""),
            ("Рабочий стол", "Desktop"),
            ("Документы", "Documents"),
            ("Загрузки", "Downloads"),
            ("Изображения", "Pictures"),
            ("Видео", "Videos"),
        ] {
            let path = if name.is_empty() {
                profile.clone()
            } else {
                profile.join(name)
            };
            if path_is_dir(&path) {
                places.push((tr(label).to_owned(), path));
            }
        }
    }
    if places.is_empty() {
        places.push((
            tr("Текущая папка").to_owned(),
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ));
    }
    places
}

fn clean_path(text: &str) -> PathBuf {
    let trimmed = text.trim().trim_matches('"').trim();
    PathBuf::from(expand_env(trimmed))
}

fn expand_env(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) => out.push_str(&value),
                    Err(_) => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn path_is_dir(path: &Path) -> bool {
    fs::metadata(crate::path_util::extended(path))
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(crate::path_util::extended(path)).is_ok()
}

fn russian_plural<'a>(count: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let last_two = count % 100;
    let last = count % 10;
    if (11..=14).contains(&last_two) {
        many
    } else if last == 1 {
        one
    } else if (2..=4).contains(&last) {
        few
    } else {
        many
    }
}

fn items_text(count: usize) -> String {
    if i18n::english() {
        if count == 1 {
            "1 item".to_owned()
        } else {
            format!("{count} items")
        }
    } else {
        format!(
            "{count} {}",
            russian_plural(count, "объект", "объекта", "объектов")
        )
    }
}

fn visible_text(visible: usize, total: usize) -> String {
    if i18n::english() {
        format!("{visible} of {total}")
    } else {
        format!("{visible} из {total}")
    }
}

fn selected_text(count: usize) -> String {
    if i18n::english() {
        if count == 1 {
            "1 selected".to_owned()
        } else {
            format!("{count} selected")
        }
    } else {
        format!(
            "Выбрано {} {}",
            count,
            russian_plural(count, "объект", "объекта", "объектов")
        )
    }
}

fn tr_msg(ru: String, en: String) -> String {
    if i18n::english() {
        en
    } else {
        ru
    }
}

fn format_size(size: u64) -> String {
    const KB: f64 = 1024.0;
    let n = size as f64;
    if n >= KB * KB * KB {
        format!("{:.2} GB", n / (KB * KB * KB))
    } else if n >= KB * KB {
        format!("{:.2} MB", n / (KB * KB))
    } else if n >= KB {
        format!("{:.1} KB", n / KB)
    } else {
        format!("{size} B")
    }
}

fn main() -> eframe::Result<()> {
    // 16 потоков ускоряют индекс, но и в простое держат лишние стеки.
    // 12 — компромисс: почти та же скорость сборки индекса, меньше потоков.
    const MAX_RAYON_THREADS: usize = 12;
    let rayon_threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .min(MAX_RAYON_THREADS);
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(rayon_threads)
        .thread_name(|index| format!("korobka-rayon-{index}"))
        .build_global();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([820.0, 540.0])
            .with_decorations(false)
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native(
        "Korobka Explorer",
        options,
        Box::new(|_cc| Ok(Box::new(KorobkaApp::new()))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_valid_docx_and_empty_zip() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "korobka_create_{}_{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&temp).unwrap();

        let docx = temp.join("Новый документ Word.docx");
        write_docx(&docx).unwrap();
        let file = fs::File::open(&docx).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert!(archive.by_name("[Content_Types].xml").is_ok());
        assert!(archive.by_name("_rels/.rels").is_ok());
        assert!(archive.by_name("word/document.xml").is_ok());

        let zip_path = temp.join("Новый архив.zip");
        write_empty_zip(&zip_path).unwrap();
        let archive = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        assert_eq!(archive.len(), 0);

        let xlsx = temp.join("Новая таблица Excel.xlsx");
        write_xlsx(&xlsx).unwrap();
        let mut archive = zip::ZipArchive::new(fs::File::open(&xlsx).unwrap()).unwrap();
        assert!(archive.by_name("xl/workbook.xml").is_ok());
        assert!(archive.by_name("xl/worksheets/sheet1.xml").is_ok());
        assert!(archive.by_name("xl/styles.xml").is_ok());

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    #[ignore]
    #[cfg(windows)]
    fn creates_windows_shortcut() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "korobka_lnk_{}_{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&temp).unwrap();
        let target = temp.join("O'Brien.txt");
        fs::write(&target, b"x").unwrap();
        let link = temp.join("O'Brien — ярлык.lnk");
        shell_shortcut(&target, &link).unwrap();
        assert!(link.is_file());
        assert!(fs::metadata(&link).unwrap().len() > 0);
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn new_file_names_are_unique() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "korobka_names_{}_{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("Новый текстовый документ.txt"), b"").unwrap();
        assert_eq!(
            unique_child_name(&temp, "Новый текстовый документ", Some("txt")),
            "Новый текстовый документ (2).txt"
        );
        assert_eq!(
            unique_child_name(&temp, "Новая папка", None),
            "Новая папка"
        );
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn filtered_selection_uses_only_visible_rows() {
        let items = vec![
            make_entry(PathBuf::from(r"C:\one.txt"), false, Some(1), None),
            make_entry(PathBuf::from(r"C:\hidden.txt"), false, Some(1), None),
            make_entry(PathBuf::from(r"C:\three.txt"), false, Some(1), None),
        ];
        let visible = [0_usize, 2];
        let mut selection = BTreeSet::new();
        let mut anchor = None;
        let mut selected = None;

        apply_selection_visible(
            &mut selection,
            &mut anchor,
            &mut selected,
            &items,
            &visible,
            0,
            egui::Modifiers::NONE,
        );
        assert_eq!(selection, BTreeSet::from([PathBuf::from(r"C:\one.txt")]));

        apply_selection_visible(
            &mut selection,
            &mut anchor,
            &mut selected,
            &items,
            &visible,
            1,
            egui::Modifiers::SHIFT,
        );
        assert_eq!(
            selection,
            BTreeSet::from([PathBuf::from(r"C:\one.txt"), PathBuf::from(r"C:\three.txt"),])
        );
        assert!(!selection.contains(&PathBuf::from(r"C:\hidden.txt")));
    }

}
