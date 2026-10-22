//! Поисковый движок: разбор запроса, совпадения и ранжирование, кэш индекса
//! тома и фоновый поток поиска.
//!
//! Запрос разбивается на слова (кавычки позволяют искать фразу с пробелами),
//! каждое слово обязано совпасть. Поддерживаются маски `*` и `?`, а слово с
//! `\` или `/` ищется по полному пути. Совпадения получают оценку, поэтому
//! точные имена и совпадения с начала имени показываются раньше.

use rayon::prelude::*;
use std::{
    cell::RefCell,
    cmp::Ordering,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
        mpsc::{self, SyncSender},
        Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

/// Формат кэша. Новый формат хранит имя и тип, а полный путь папки — один раз
/// на блок, поэтому индекс занимает в 3–4 раза меньше места.
pub(crate) const CACHE_HEADER: &str = "KOROBKA4";
/// Старые форматы с полным путём в каждой строке тоже читаем.
const CACHE_HEADERS_FULL: [&[u8]; 2] = [b"KOROBKA2", b"KOROBKA3"];
/// Кэш читается крупными блоками: меньше вызовов и меньше накладных расходов
/// на rayon-разбор.
const CACHE_CHUNK: usize = 4 << 20;
const BATCH_SIZE: usize = 512;
const INDEX_CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_INDEX_ENTRIES: usize = 20_000_000;
static SEARCH_TMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchMode {
    ExactName,
    Contains,
}

impl SearchMode {
    pub(crate) fn from_index(index: usize) -> Self {
        if index == 0 {
            Self::ExactName
        } else {
            Self::Contains
        }
    }
}

#[derive(Clone)]
pub(crate) struct SearchHit {
    pub(crate) path: PathBuf,
    pub(crate) name: Box<str>,
    pub(crate) is_dir: bool,
    /// Чем меньше, тем выше результат в выдаче.
    score: u8,
    parent_text: OnceLock<Box<str>>,
}

impl SearchHit {
    fn new(path: PathBuf, name: &str, is_dir: bool, score: u8) -> Self {
        Self {
            path,
            name: name.into(),
            is_dir,
            score,
            parent_text: OnceLock::new(),
        }
    }

    /// Папка результата форматируется один раз, а не на каждый кадр.
    pub(crate) fn parent_text(&self) -> &str {
        self.parent_text.get_or_init(|| {
            self.path
                .parent()
                .map(|parent| parent.display().to_string().into_boxed_str())
                .unwrap_or_default()
        })
    }

    pub(crate) fn rank_cmp(&self, other: &Self) -> Ordering {
        self.score
            .cmp(&other.score)
            .then_with(|| self.name.len().cmp(&other.name.len()))
            .then_with(|| self.name.cmp(&other.name))
    }
}

pub(crate) enum SearchMessage {
    IndexInfo {
        from_cache: bool,
    },
    IndexProgress {
        scanned: u64,
    },
    Batch {
        hits: Vec<SearchHit>,
        scanned: u64,
    },
    Finished {
        scanned: u64,
        elapsed: Duration,
    },
    Failed(String),
}

// ---------------------------------------------------------------------------
// Разбор и сопоставление запроса
// ---------------------------------------------------------------------------

thread_local! {
    static LOWER_SCRATCH: RefCell<String> = const { RefCell::new(String::new()) };
}

fn lowercase(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

/// Приводит строку к нижнему регистру во временном буфере потока: поиск не
/// аллоцирует память на каждое имя.
fn with_lowered<T>(value: &str, f: impl FnOnce(&str) -> T) -> T {
    LOWER_SCRATCH.with(|cell| {
        let mut buffer = cell.borrow_mut();
        buffer.clear();
        buffer.reserve(value.len());
        for ch in value.chars() {
            if ch.is_ascii() {
                buffer.push(ch.to_ascii_lowercase());
            } else {
                for lower in ch.to_lowercase() {
                    buffer.push(lower);
                }
            }
        }
        f(&buffer)
    })
}

fn is_boundary(value: &str, position: usize) -> bool {
    value[..position]
        .chars()
        .next_back()
        .is_none_or(|ch| !ch.is_alphanumeric())
}

/// Поиск ASCII-подстроки без учёта регистра. `memchr2` перебирает только
/// позиции, где может начинаться совпадение, а не каждый байт.
fn find_ascii_ignore_case(value: &str, needle: &str) -> Option<usize> {
    let haystack = value.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    let first = needle[0];
    let lower = first.to_ascii_lowercase();
    let upper = first.to_ascii_uppercase();
    let last_start = haystack.len() - needle.len();
    let mut start = 0;
    loop {
        let offset = memchr::memchr2(lower, upper, &haystack[start..=last_start])?;
        let position = start + offset;
        if haystack[position..position + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(position);
        }
        start = position + 1;
        if start > last_start {
            return None;
        }
    }
}

#[cfg(test)]
pub(crate) fn contains_ascii_ignore_case(haystack: &str, needle: &str) -> bool {
    find_ascii_ignore_case(haystack, needle).is_some()
}

fn eq_ignore_case(value: &str, needle_lower: &str) -> bool {
    if value.eq_ignore_ascii_case(needle_lower) {
        return true;
    }
    if value.is_ascii() {
        return false;
    }
    with_lowered(value, |lower| lower == needle_lower)
}

/// Оценка совпадения одного слова: 0 — точное имя, 1 — начало имени,
/// 2 — начало слова, 3 — вхождение внутри имени. Один проход по строке.
fn literal_rank(value: &str, needle: &str) -> Option<u8> {
    if needle.is_empty() {
        return Some(3);
    }
    if needle.is_ascii() {
        let position = find_ascii_ignore_case(value, needle)?;
        Some(if position == 0 {
            if value.len() == needle.len() {
                0
            } else {
                1
            }
        } else if is_boundary(value, position) {
            2
        } else {
            3
        })
    } else {
        with_lowered(value, |lower| {
            let position = lower.find(needle)?;
            Some(if position == 0 {
                if lower.len() == needle.len() {
                    0
                } else {
                    1
                }
            } else if is_boundary(lower, position) {
                2
            } else {
                3
            })
        })
    }
}

/// Классический glob-матчер с откатом. `haystack` уже в нижнем регистре,
/// `pattern` тоже; `?` совпадает с одним символом.
fn glob_match(haystack: &str, pattern: &[char]) -> bool {
    let mut position = 0;
    let mut pattern_pos = 0;
    let mut star: Option<(usize, usize)> = None;

    while position < haystack.len() {
        let ch = haystack[position..].chars().next().unwrap_or('\0');
        if pattern_pos < pattern.len() {
            let token = pattern[pattern_pos];
            if token == '*' {
                star = Some((pattern_pos + 1, position));
                pattern_pos += 1;
                continue;
            }
            if token == '?' || token == ch {
                position += ch.len_utf8();
                pattern_pos += 1;
                continue;
            }
        }
        if let Some((star_pattern, star_position)) = star {
            let next = star_position
                + haystack[star_position..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(1);
            if next > haystack.len() {
                return false;
            }
            star = Some((star_pattern, next));
            position = next;
            pattern_pos = star_pattern;
        } else {
            return false;
        }
    }

    while pattern_pos < pattern.len() && pattern[pattern_pos] == '*' {
        pattern_pos += 1;
    }
    pattern_pos == pattern.len()
}

struct Term {
    text: String,
    pattern: Vec<char>,
    pattern_contains: Vec<char>,
    wildcard: bool,
    /// Шаблон с `*` в начале или конце считается маской всего имени,
    /// иначе звёздочки могут встретиться в любом месте имени.
    anchored: bool,
    path: bool,
}

impl Term {
    fn new(raw: &str) -> Self {
        let text = lowercase(raw);
        let wildcard = text.contains('*') || text.contains('?');
        let anchored = wildcard && (text.starts_with('*') || text.ends_with('*'));
        let pattern = if wildcard {
            text.chars().collect()
        } else {
            Vec::new()
        };
        let pattern_contains = if wildcard && !anchored {
            let mut padded = Vec::with_capacity(pattern.len() + 2);
            padded.push('*');
            padded.extend_from_slice(&pattern);
            padded.push('*');
            padded
        } else {
            Vec::new()
        };
        Self {
            path: text.contains('\\') || text.contains('/'),
            wildcard,
            anchored,
            pattern,
            pattern_contains,
            text,
        }
    }

    fn pattern(&self) -> &[char] {
        if self.wildcard && !self.anchored {
            &self.pattern_contains
        } else {
            &self.pattern
        }
    }

    fn rank(&self, name: &str, path: Option<&str>) -> Option<u8> {
        let pattern = self.pattern();
        let name_rank = if self.wildcard {
            with_lowered(name, |lower| glob_match(lower, pattern)).then_some(3)
        } else {
            literal_rank(name, &self.text)
        };
        if name_rank.is_some() || !self.path {
            return name_rank;
        }
        let path = path?;
        if self.wildcard {
            with_lowered(path, |lower| glob_match(lower, pattern)).then_some(5)
        } else {
            literal_rank(path, &self.text).map(|_| 5)
        }
    }
}

fn tokenize(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in query.chars() {
        match ch {
            '"' => quoted = !quoted,
            ch if ch.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    terms.push(std::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms
}

pub(crate) struct SearchQuery {
    mode: SearchMode,
    terms: Vec<Term>,
    needle: String,
    pattern: Vec<char>,
    wildcard: bool,
}

impl SearchQuery {
    pub(crate) fn new(query: &str, mode: SearchMode) -> Self {
        let trimmed = query.trim();
        // Кавычки нужны только токенизатору; в само имя они не входят.
        let needle = if trimmed.contains('"') {
            lowercase(&trimmed.replace('"', ""))
        } else {
            lowercase(trimmed)
        };
        let wildcard = needle.contains('*') || needle.contains('?');
        let pattern = if wildcard {
            needle.chars().collect()
        } else {
            Vec::new()
        };
        let terms = match mode {
            SearchMode::Contains => tokenize(trimmed)
                .iter()
                .map(|term| Term::new(term))
                .collect(),
            SearchMode::ExactName => Vec::new(),
        };
        Self {
            mode,
            terms,
            needle,
            pattern,
            wildcard,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self.mode {
            SearchMode::ExactName => self.needle.is_empty(),
            SearchMode::Contains => self.terms.is_empty(),
        }
    }

    pub(crate) fn needs_path(&self) -> bool {
        self.terms.iter().any(|term| term.path)
    }

    pub(crate) fn matches(&self, name: &str, path: Option<&str>) -> bool {
        self.score(name, path).is_some()
    }

    fn score(&self, name: &str, path: Option<&str>) -> Option<u8> {
        match self.mode {
            SearchMode::ExactName => {
                if self.needle.is_empty() {
                    return Some(3);
                }
                if self.wildcard {
                    if with_lowered(name, |lower| glob_match(lower, &self.pattern)) {
                        Some(0)
                    } else {
                        None
                    }
                } else if eq_ignore_case(name, &self.needle) {
                    Some(0)
                } else {
                    None
                }
            }
            SearchMode::Contains => {
                if self.terms.len() == 1 {
                    return self.terms[0].rank(name, path);
                }
                if self.terms.is_empty() {
                    return Some(3);
                }
                let mut rank = 0_u8;
                for term in &self.terms {
                    rank = rank.max(term.rank(name, path)?);
                }
                Some(rank)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Кэш индекса
// ---------------------------------------------------------------------------

fn index_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Korobka")
        .join("index")
}

fn generation_path() -> PathBuf {
    index_dir().join(".generation")
}

/// Помечает все кэши индекса устаревшими после файловых операций. Это дешевле,
/// чем удалять каждый файл кэша, и автоматически действует на все тома.
pub(crate) fn touch_generation() {
    let _ = fs::create_dir_all(index_dir());
    let _ = fs::write(generation_path(), b"1");
}

fn cache_newer_than_generation(cache: &Path) -> bool {
    let Ok(generation) = fs::metadata(generation_path()).and_then(|meta| meta.modified()) else {
        return true;
    };
    let Ok(cache_time) = fs::metadata(cache).and_then(|meta| meta.modified()) else {
        return true;
    };
    cache_time > generation
}

pub(crate) fn cache_is_fresh(cache: &Path) -> bool {
    let fresh_age = fs::metadata(cache)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age <= INDEX_CACHE_MAX_AGE);
    fresh_age && cache_newer_than_generation(cache)
}

/// Убирает старые кэши и брошенные временные файлы, чтобы папка индекса не
/// росла бесконечно после поисков по разным папкам и томам.
fn prune_index_dir() {
    let Ok(entries) = fs::read_dir(index_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        let limit = if extension == "idx" {
            INDEX_CACHE_MAX_AGE
        } else if extension.starts_with("tmp") {
            Duration::from_secs(24 * 60 * 60)
        } else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = modified.elapsed() else {
            continue;
        };
        if age > limit {
            let _ = fs::remove_file(path);
        }
    }
}

enum CacheStream {
    Done,
    Invalid,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Фоновый поиск
// ---------------------------------------------------------------------------

pub(crate) fn search_worker(
    root: PathBuf,
    cache: Option<PathBuf>,
    query: String,
    mode: SearchMode,
    cancel: Arc<AtomicBool>,
    sender: SyncSender<SearchMessage>,
    started: Instant,
) {
    let matcher = SearchQuery::new(&query, mode);
    if let Some(cache_path) = cache.as_deref() {
        if cache_is_fresh(cache_path) {
            match stream_cache(cache_path, &matcher, &cancel, &sender, started) {
                CacheStream::Done | CacheStream::Cancelled => return,
                CacheStream::Invalid => {
                    let _ = fs::remove_file(cache_path);
                }
            }
        }
    }
    build_and_search(&root, cache.as_deref(), &matcher, &cancel, &sender, started);
}

fn parse_and_score(line: &[u8], matcher: &SearchQuery) -> Option<SearchHit> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let flag = *line.first()?;
    if line.get(1) != Some(&b'\t') {
        return None;
    }
    let is_dir = match flag {
        b'D' => true,
        b'F' => false,
        _ => return None,
    };
    let path = std::str::from_utf8(&line[2..]).ok()?;
    if path.is_empty() {
        return None;
    }
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let score = matcher.score(name, Some(path))?;
    Some(SearchHit::new(PathBuf::from(path), name, is_dir, score))
}

fn index_read_error(error: &std::io::Error) -> String {
    if crate::i18n::english() {
        format!("Could not read index: {error}")
    } else {
        format!("Не удалось прочитать индекс: {error}")
    }
}

fn join_child(dir: &str, name: &str) -> String {
    let mut path = String::with_capacity(dir.len() + name.len() + 1);
    path.push_str(dir);
    if !dir.ends_with('\\') && !dir.ends_with('/') {
        path.push('\\');
    }
    path.push_str(name);
    path
}

/// Компактный кэш: `B\t<папка>`, затем `d\t<имя>` и `f\t<имя>`.
/// Полный путь папки хранится один раз, поэтому файл в разы меньше.
fn stream_compact_cache(
    mut reader: BufReader<fs::File>,
    matcher: &SearchQuery,
    cancel: &AtomicBool,
    sender: &SyncSender<SearchMessage>,
    started: Instant,
) -> CacheStream {
    let mut scanned = 0_u64;
    let mut pending = Vec::with_capacity(BATCH_SIZE);
    let mut buffer = Vec::with_capacity(CACHE_CHUNK * 2);
    let mut chunk = vec![0_u8; CACHE_CHUNK];
    let mut current_dir: Option<String> = None;
    let mut chunks = 0_u32;

    loop {
        let read = match reader.read(&mut chunk) {
            Ok(read) => read,
            Err(error) => {
                let _ = sender.send(SearchMessage::Failed(index_read_error(&error)));
                return CacheStream::Cancelled;
            }
        };
        if cancel.load(AtomicOrdering::Relaxed) {
            return CacheStream::Cancelled;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let eof = read == 0;
        let end = if eof {
            buffer.len()
        } else {
            buffer
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|position| position + 1)
                .unwrap_or(0)
        };
        if end > 0 {
            let region = &buffer[..end];
            for line in region.split(|byte| *byte == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.len() < 2 || line[1] != b'\t' {
                    continue;
                }
                let flag = line[0];
                let payload = &line[2..];
                match flag {
                    b'B' => {
                        if let Ok(dir) = std::str::from_utf8(payload) {
                            current_dir = Some(dir.to_owned());
                        }
                    }
                    b'd' | b'f' => {
                        let Ok(name) = std::str::from_utf8(payload) else {
                            continue;
                        };
                        if name.is_empty() {
                            continue;
                        }
                        scanned += 1;
                        let Some(dir) = current_dir.as_deref() else {
                            continue;
                        };
                        let full_path = if matcher.needs_path() {
                            Some(join_child(dir, name))
                        } else {
                            None
                        };
                        if let Some(score) = matcher.score(name, full_path.as_deref()) {
                            let path = full_path.unwrap_or_else(|| join_child(dir, name));
                            pending.push(SearchHit::new(
                                PathBuf::from(path),
                                name,
                                flag == b'd',
                                score,
                            ));
                            if pending.len() >= BATCH_SIZE {
                                let message = SearchMessage::Batch {
                                    hits: std::mem::take(&mut pending),
                                    scanned,
                                };
                                if sender.send(message).is_err() {
                                    return CacheStream::Cancelled;
                                }
                                pending = Vec::with_capacity(BATCH_SIZE);
                            }
                        }
                    }
                    _ => {}
                }
            }
            buffer.drain(..end);
            chunks += 1;
            if chunks.is_multiple_of(4)
                && sender
                    .send(SearchMessage::IndexProgress { scanned })
                    .is_err()
            {
                return CacheStream::Cancelled;
            }
        }
        if eof {
            break;
        }
    }

    if !pending.is_empty() {
        let message = SearchMessage::Batch {
            hits: std::mem::take(&mut pending),
            scanned,
        };
        if sender.send(message).is_err() {
            return CacheStream::Cancelled;
        }
    }
    let _ = sender.send(SearchMessage::Finished {
        scanned,
        elapsed: started.elapsed(),
    });
    CacheStream::Done
}

fn stream_cache(
    cache: &Path,
    matcher: &SearchQuery,
    cancel: &AtomicBool,
    sender: &SyncSender<SearchMessage>,
    started: Instant,
) -> CacheStream {
    let Ok(file) = fs::File::open(cache) else {
        return CacheStream::Invalid;
    };
    let mut reader = BufReader::with_capacity(CACHE_CHUNK, file);
    let mut header = Vec::new();
    if reader.read_until(b'\n', &mut header).is_err() {
        return CacheStream::Invalid;
    }
    let header = header.strip_suffix(b"\n").unwrap_or(&header);
    let header = header.strip_suffix(b"\r").unwrap_or(header);
    let compact = header == CACHE_HEADER.as_bytes();
    if !compact && !CACHE_HEADERS_FULL.contains(&header) {
        return CacheStream::Invalid;
    }
    if sender
        .send(SearchMessage::IndexInfo { from_cache: true })
        .is_err()
    {
        return CacheStream::Cancelled;
    }
    if compact {
        return stream_compact_cache(reader, matcher, cancel, sender, started);
    }

    let mut scanned = 0_u64;
    let mut pending = Vec::with_capacity(BATCH_SIZE);
    let mut buffer = Vec::with_capacity(CACHE_CHUNK * 2);
    let mut chunk = vec![0_u8; CACHE_CHUNK];
    let mut chunks = 0_u32;

    loop {
        let read = match reader.read(&mut chunk) {
            Ok(read) => read,
            Err(error) => {
                let _ = sender.send(SearchMessage::Failed(index_read_error(&error)));
                return CacheStream::Cancelled;
            }
        };
        if cancel.load(AtomicOrdering::Relaxed) {
            return CacheStream::Cancelled;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let eof = read == 0;
        let end = if eof {
            buffer.len()
        } else {
            buffer
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|position| position + 1)
                .unwrap_or(0)
        };
        if end > 0 {
            let region = &buffer[..end];
            scanned += region.iter().filter(|byte| **byte == b'\n').count() as u64;
            if eof && !region.ends_with(b"\n") {
                scanned += 1;
            }
            let hits: Vec<SearchHit> = region
                .par_split(|byte| *byte == b'\n')
                .filter_map(|line| parse_and_score(line, matcher))
                .collect();
            pending.extend(hits);
            if pending.len() >= BATCH_SIZE {
                let message = SearchMessage::Batch {
                    hits: std::mem::take(&mut pending),
                    scanned,
                };
                if sender.send(message).is_err() {
                    return CacheStream::Cancelled;
                }
                pending = Vec::with_capacity(BATCH_SIZE);
            }
            buffer.drain(..end);
            chunks += 1;
            if chunks.is_multiple_of(4)
                && sender
                    .send(SearchMessage::IndexProgress { scanned })
                    .is_err()
            {
                return CacheStream::Cancelled;
            }
        }
        if eof {
            break;
        }
    }

    if !pending.is_empty() {
        let message = SearchMessage::Batch {
            hits: std::mem::take(&mut pending),
            scanned,
        };
        if sender.send(message).is_err() {
            return CacheStream::Cancelled;
        }
    }
    let _ = sender.send(SearchMessage::Finished {
        scanned,
        elapsed: started.elapsed(),
    });
    CacheStream::Done
}

struct BuildState<'a> {
    matcher: &'a SearchQuery,
    cancel: &'a AtomicBool,
    stop: &'a AtomicBool,
    scanned: &'a AtomicU64,
    sender: &'a SyncSender<SearchMessage>,
    lines: Option<&'a SyncSender<Vec<u8>>>,
}

/// Пишет кэш в отдельном потоке: обход дерева идёт параллельно, а запись
/// остаётся последовательной.
fn write_cache(
    path: PathBuf,
    receiver: std::sync::mpsc::Receiver<Vec<u8>>,
) -> bool {
    let file = match fs::File::create(path) {
        Ok(file) => file,
        Err(_) => {
            while receiver.recv().is_ok() {}
            return false;
        }
    };
    let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
    let mut ok = out.write_all(CACHE_HEADER.as_bytes()).is_ok()
        && out.write_all(b"\n").is_ok();
    while let Ok(chunk) = receiver.recv() {
        if ok && out.write_all(&chunk).is_err() {
            ok = false;
        }
    }
    if ok && out.flush().is_err() {
        ok = false;
    }
    ok
}

/// Читает имена и типы элементов каталога без лишних обращений к метаданным.
#[cfg(windows)]
fn read_dir_entries(
    path: &Path,
) -> std::io::Result<Vec<(std::ffi::OsString, bool, bool)>> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH,
            WIN32_FIND_DATAW,
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
            let is_dir = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
            let reparse = data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            entries.push((name, is_dir, reparse));
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

#[cfg(not(windows))]
fn read_dir_entries(
    path: &Path,
) -> std::io::Result<Vec<(std::ffi::OsString, bool, bool)>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(path)? {
        let Ok(item) = item else { continue };
        let Ok(file_type) = item.file_type() else { continue };
        entries.push((item.file_name(), file_type.is_dir(), file_type.is_symlink()));
    }
    Ok(entries)
}

fn visit_dir(dir: PathBuf, state: &BuildState, is_root: bool) -> bool {
    if state.cancel.load(AtomicOrdering::Relaxed) || state.stop.load(AtomicOrdering::Relaxed) {
        return true;
    }
    let entries = match read_dir_entries(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            if is_root {
                let _ = state.sender.send(SearchMessage::Failed(if crate::i18n::english() {
                    format!("Could not read {}: {error}", dir.display())
                } else {
                    format!("Не удалось прочитать {}: {error}", dir.display())
                }));
                return false;
            }
            return true;
        }
    };

    let mut subdirs = Vec::new();
    let mut block = String::with_capacity(8 * 1024);
    if state.lines.is_some() {
        // Блок начинается с полного пути папки, дальше — только имена детей.
        block.push('B');
        block.push('\t');
        block.push_str(&dir.to_string_lossy());
        block.push('\n');
    }
    let mut hits = Vec::new();

    for (name, is_dir, reparse) in entries {
        if state.cancel.load(AtomicOrdering::Relaxed) {
            return true;
        }
        if state.stop.load(AtomicOrdering::Relaxed) {
            break;
        }
        let path = dir.join(&name);
        let name = name.to_string_lossy();
        let total = state.scanned.fetch_add(1, AtomicOrdering::Relaxed) + 1;
        if total as usize >= MAX_INDEX_ENTRIES {
            state.stop.store(true, AtomicOrdering::Relaxed);
        }
        let path_text = if state.matcher.needs_path() {
            Some(path.to_string_lossy())
        } else {
            None
        };
        if state.lines.is_some() {
            block.push(if is_dir { 'd' } else { 'f' });
            block.push('\t');
            block.push_str(name.as_ref());
            block.push('\n');
        }
        if let Some(score) = state.matcher.score(name.as_ref(), path_text.as_deref()) {
            hits.push(SearchHit::new(path.clone(), name.as_ref(), is_dir, score));
        }
        if is_dir && !reparse {
            subdirs.push(path);
        }
        if total.is_multiple_of(50_000)
            && state
                .sender
                .send(SearchMessage::IndexProgress { scanned: total })
                .is_err()
        {
            state.cancel.store(true, AtomicOrdering::Relaxed);
            return true;
        }
    }

    if let Some(lines) = state.lines {
        if lines.send(block.into_bytes()).is_err() {
            state.cancel.store(true, AtomicOrdering::Relaxed);
            return true;
        }
    }

    let scanned = state.scanned.load(AtomicOrdering::Relaxed);
    while hits.len() > BATCH_SIZE {
        let rest = hits.split_off(BATCH_SIZE);
        if state
            .sender
            .send(SearchMessage::Batch { hits, scanned })
            .is_err()
        {
            state.cancel.store(true, AtomicOrdering::Relaxed);
            return true;
        }
        hits = rest;
    }
    if !hits.is_empty()
        && state
            .sender
            .send(SearchMessage::Batch { hits, scanned })
            .is_err()
    {
        state.cancel.store(true, AtomicOrdering::Relaxed);
        return true;
    }

    subdirs
        .into_par_iter()
        .for_each(|subdir| {
            visit_dir(subdir, state, false);
        });
    true
}

fn build_and_search(
    root: &Path,
    cache: Option<&Path>,
    matcher: &SearchQuery,
    cancel: &AtomicBool,
    sender: &SyncSender<SearchMessage>,
    started: Instant,
) {
    if cache.is_some()
        && sender
            .send(SearchMessage::IndexInfo { from_cache: false })
            .is_err()
    {
        return;
    }

    let temp_path = cache.map(|cache_path| {
        cache_path.with_extension(format!(
            "tmp{}_{}",
            std::process::id(),
            SEARCH_TMP_ID.fetch_add(1, AtomicOrdering::Relaxed)
        ))
    });
    if let Some(cache_path) = cache {
        if let Some(parent) = cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
    }

    let (line_tx, writer_handle) = match temp_path.as_deref() {
        Some(temp) => {
            let temp = temp.to_path_buf();
            let (line_tx, line_rx) = mpsc::sync_channel::<Vec<u8>>(64);
            match thread::Builder::new()
                .name("korobka-index-writer".to_owned())
                .spawn(move || write_cache(temp, line_rx))
            {
                Ok(handle) => (Some(line_tx), Some(handle)),
                Err(_) => {
                    drop(line_tx);
                    (None, None)
                }
            }
        }
        None => (None, None),
    };

    let scanned = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let state = BuildState {
        matcher,
        cancel,
        stop: &stop,
        scanned: &scanned,
        sender,
        lines: line_tx.as_ref(),
    };

    let root_ok = visit_dir(root.to_path_buf(), &state, true);

    drop(line_tx);
    let writer_ok = match writer_handle {
        Some(handle) => handle.join().unwrap_or(false),
        None => false,
    };
    let complete = !stop.load(AtomicOrdering::Relaxed);
    let scanned = scanned.load(AtomicOrdering::Relaxed);

    if !root_ok || cancel.load(AtomicOrdering::Relaxed) {
        if let Some(temp) = temp_path.as_deref() {
            let _ = fs::remove_file(temp);
        }
        return;
    }

    if writer_ok && complete {
        if let (Some(cache_path), Some(temp)) = (cache, temp_path.as_deref()) {
            let _ = fs::remove_file(cache_path);
            if fs::rename(temp, cache_path).is_err() {
                let _ = fs::remove_file(temp);
            }
        }
    } else if let Some(temp) = temp_path.as_deref() {
        let _ = fs::remove_file(temp);
    }
    if cache.is_some() && complete {
        prune_index_dir();
    }

    let _ = sender.send(SearchMessage::Finished {
        scanned,
        elapsed: started.elapsed(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn run_search(
        root: &Path,
        cache: Option<&Path>,
        query: &str,
        mode: SearchMode,
    ) -> (bool, Vec<SearchHit>, u64) {
        let (tx, rx) = mpsc::sync_channel(256);
        search_worker(
            root.to_path_buf(),
            cache.map(Path::to_path_buf),
            query.to_owned(),
            mode,
            Arc::new(AtomicBool::new(false)),
            tx,
            Instant::now(),
        );
        let mut from_cache = false;
        let mut hits = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(SearchMessage::IndexInfo { from_cache: cached }) => from_cache = cached,
                Ok(SearchMessage::IndexProgress { .. }) => {}
                Ok(SearchMessage::Batch { hits: batch, .. }) => hits.extend(batch),
                Ok(SearchMessage::Finished { scanned, .. }) => {
                    return (from_cache, hits, scanned);
                }
                Ok(SearchMessage::Failed(error)) => panic!("search failed: {error}"),
                Err(error) => panic!("search timed out: {error}"),
            }
        }
    }

    fn temp_tree() -> (PathBuf, PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp =
            std::env::temp_dir().join(format!("korobka_search_{}_{}", std::process::id(), stamp));
        let root = temp.join("tree");
        fs::create_dir_all(root.join("Папка")).unwrap();
        fs::write(root.join("Отчёт 2026.TXT"), b"x").unwrap();
        fs::write(root.join("other.txt"), b"x").unwrap();
        fs::write(root.join("Папка").join("вложенный.txt"), b"x").unwrap();
        (temp, root)
    }

    #[test]
    fn ascii_case_insensitive_contains() {
        assert!(contains_ascii_ignore_case("Needle_alpha.txt", "needle"));
        assert!(contains_ascii_ignore_case("NEEDLE.txt", "needle"));
        assert!(!contains_ascii_ignore_case("other.txt", "needle"));
        assert!(!contains_ascii_ignore_case("needle", "needle_alpha"));
        assert!(contains_ascii_ignore_case("файл need.txt", "need"));
    }

    #[test]
    fn unicode_case_insensitive_search() {
        let query = SearchQuery::new("ПАПКА", SearchMode::Contains);
        assert!(query.matches("папка", None));
        assert!(query.matches("Моя Папка", None));
        assert!(!query.matches("файл.txt", None));

        let exact = SearchQuery::new("отчёт 2026.txt", SearchMode::ExactName);
        assert!(exact.matches("Отчёт 2026.TXT", None));
        assert!(!exact.matches("Отчёт 2026.TXT.bak", None));
    }

    #[test]
    fn multiple_terms_are_and() {
        let query = SearchQuery::new("отчёт txt", SearchMode::Contains);
        assert!(query.matches("Отчёт 2026.txt", None));
        assert!(!query.matches("Отчёт 2026.pdf", None));
    }

    #[test]
    fn wildcards_and_paths() {
        let mask = SearchQuery::new("*.txt", SearchMode::Contains);
        assert!(mask.matches("readme.txt", None));
        assert!(!mask.matches("readme.md", None));

        let single = SearchQuery::new("отч?т", SearchMode::Contains);
        assert!(single.matches("Отчёт.txt", None));
        assert!(!single.matches("файл.pdf", Some(r"C:\other\Отчёт.txt")));

        let path = SearchQuery::new(r"users\ivan\doc", SearchMode::Contains);
        assert!(path.matches("file.txt", Some(r"C:\Users\Ivan\Documents\file.txt")));
        assert!(!path.matches("file.txt", Some(r"C:\Users\Oleg\file.txt")));
        assert!(path.needs_path());
    }

    #[test]
    fn ranking_prefers_exact_and_prefix() {
        assert_eq!(literal_rank("report.txt", "report.txt"), Some(0));
        assert_eq!(literal_rank("Report-final.txt", "report"), Some(1));
        assert_eq!(literal_rank("my-report.txt", "report"), Some(2));
        assert_eq!(literal_rank("myreport.txt", "report"), Some(3));
        assert_eq!(literal_rank("other.txt", "report"), None);
    }

    #[test]
    fn streams_index_and_reuses_cache() {
        let (temp, root) = temp_tree();
        let cache = temp.join("index.cache");

        let (from_cache, hits, scanned) =
            run_search(&root, Some(&cache), "отчёт", SearchMode::Contains);
        assert!(!from_cache);
        assert_eq!(scanned, 4);
        assert!(hits
            .iter()
            .any(|hit| hit.name.as_ref() == "Отчёт 2026.TXT" && !hit.is_dir));

        let header = fs::read_to_string(&cache).unwrap();
        assert!(header.starts_with(CACHE_HEADER));

        let (from_cache, hits, _) =
            run_search(&root, Some(&cache), "пап", SearchMode::Contains);
        assert!(from_cache);
        assert!(hits
            .iter()
            .any(|hit| hit.is_dir && hit.name.as_ref() == "Папка"));

        let (from_cache, hits, _) =
            run_search(&root, Some(&cache), "*.txt", SearchMode::Contains);
        assert!(from_cache);
        assert_eq!(hits.len(), 3);

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn reads_legacy_full_path_cache() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "korobka_legacy_{}_{}",
            std::process::id(),
            stamp
        ));
        fs::create_dir_all(&temp).unwrap();
        let cache = temp.join("legacy.idx");
        fs::write(
            &cache,
            b"KOROBKA3\nD\tC:\\legacy\\sub\nF\tC:\\legacy\\foo.txt\nF\tC:\\legacy\\bar.txt\n",
        )
        .unwrap();

        let (from_cache, hits, scanned) =
            run_search(Path::new(r"C:\"), Some(&cache), "foo", SearchMode::Contains);
        assert!(from_cache);
        assert_eq!(scanned, 3);
        assert!(hits
            .iter()
            .any(|hit| hit.path.as_path() == Path::new(r"C:\legacy\foo.txt") && !hit.is_dir));

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn finds_files_beyond_max_path() {
        let (temp, root) = temp_tree();
        let mut long = root.clone();
        for _ in 0..8 {
            long.push("d".repeat(40));
        }
        fs::create_dir_all(crate::path_util::extended(&long)).unwrap();
        let file = long.join("deep-long.txt");
        fs::write(crate::path_util::extended(&file), b"x").unwrap();
        assert!(file.to_string_lossy().len() > 260);

        let (_, hits, _) = run_search(&root, None, "deep-long", SearchMode::Contains);
        assert!(hits.iter().any(|hit| hit.path == file));

        fs::remove_dir_all(crate::path_util::extended(&temp)).unwrap();
    }

    #[test]
    fn direct_walk_without_cache() {
        let (temp, root) = temp_tree();
        let (from_cache, hits, scanned) = run_search(&root, None, "txt", SearchMode::Contains);
        assert!(!from_cache);
        assert_eq!(scanned, 4);
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|hit| !hit.is_dir));
        assert!(!temp.join("index.cache").exists());
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn parallel_walk_builds_deep_cache() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp =
            std::env::temp_dir().join(format!("korobka_parallel_{}_{}", std::process::id(), stamp));
        let root = temp.join("tree");
        for level in 0..16 {
            let dir = root.join(format!("level{level}")).join("nested");
            fs::create_dir_all(&dir).unwrap();
            for file in 0..16 {
                fs::write(dir.join(format!("target_{level}_{file}.dat")), b"x").unwrap();
            }
        }
        let cache = temp.join("index.cache");

        let (from_cache, hits, scanned) =
            run_search(&root, Some(&cache), "target", SearchMode::Contains);
        assert!(!from_cache);
        assert_eq!(scanned, 16 * 16 + 16 * 2);
        assert_eq!(hits.len(), 16 * 16);

        let (from_cache, hits, scanned) =
            run_search(&root, Some(&cache), "target", SearchMode::Contains);
        assert!(from_cache);
        assert_eq!(scanned, 16 * 16 + 16 * 2);
        assert_eq!(hits.len(), 16 * 16);

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn results_are_ranked() {
        let mut hits = [
            SearchHit::new(PathBuf::from(r"C:\a\report.txt"), "report.txt", false, 0),
            SearchHit::new(PathBuf::from(r"C:\b\myreport.txt"), "myreport.txt", false, 3),
            SearchHit::new(PathBuf::from(r"C:\c\report-final.txt"), "report-final.txt", false, 1),
        ];
        hits.sort_unstable_by(SearchHit::rank_cmp);
        let names: Vec<&str> = hits.iter().map(|hit| hit.name.as_ref()).collect();
        assert_eq!(names, ["report.txt", "report-final.txt", "myreport.txt"]);
    }

    #[test]
    fn glob_matches_empty_and_trailing_star() {
        let pattern = |value: &str| value.chars().collect::<Vec<_>>();
        assert!(glob_match("", &pattern("*")));
        assert!(glob_match("abc", &pattern("ab*")));
        assert!(glob_match("abc", &pattern("*bc")));
        assert!(glob_match("abc", &pattern("a*c")));
        assert!(!glob_match("abc", &pattern("a*d")));
        assert!(glob_match("abc", &pattern("???")));
        assert!(!glob_match("abc", &pattern("??")));
    }
}
