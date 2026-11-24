use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_CACHE_LIMIT_BYTES: u64 = 500 * 1024 * 1024;
static CACHE_LIMIT_BYTES: AtomicU64 = AtomicU64::new(DEFAULT_CACHE_LIMIT_BYTES);

pub fn get_cache_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join("PDFForge").join("cache");
    let _ = fs::create_dir_all(root.join("renders"));
    let _ = fs::create_dir_all(root.join("thumbs"));
    let _ = fs::create_dir_all(root.join("search_index"));
    root
}

pub fn set_disk_cache_limit_mb(mb: u32) {
    let bytes = (mb.clamp(50, 4096) as u64) * 1024 * 1024;
    CACHE_LIMIT_BYTES.store(bytes, Ordering::Relaxed);
    std::thread::spawn(enforce_disk_cache_limit);
}

pub fn get_disk_cache_limit_mb() -> u32 {
    (CACHE_LIMIT_BYTES.load(Ordering::Relaxed) / (1024 * 1024)) as u32
}

/// Fast O(1) file fingerprint hash (<0.5ms even on 500MB PDFs): hashes size, mtime, first 64KB, and last 64KB.
pub fn compute_file_hash(file_path: &str) -> String {
    let Ok(mut f) = fs::File::open(file_path) else {
        return format!("{:016x}", fnv1a_bytes(file_path.as_bytes(), 0xcbf29ce484222325));
    };
    let meta = match f.metadata() {
        Ok(m) => m,
        Err(_) => {
            return format!("{:016x}", fnv1a_bytes(file_path.as_bytes(), 0xcbf29ce484222325));
        }
    };
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut h: u64 = 0xcbf29ce484222325;
    h = fnv1a_bytes(&len.to_le_bytes(), h);
    h = fnv1a_bytes(&mtime.to_le_bytes(), h);

    let mut buf = vec![0u8; 65536.min(len as usize)];
    if !buf.is_empty() {
        if let Ok(n) = f.read(&mut buf) {
            h = fnv1a_bytes(&buf[..n], h);
        }
    }
    if len > 65536 {
        let tail_start = len.saturating_sub(65536);
        if f.seek(SeekFrom::Start(tail_start)).is_ok() {
            let mut tail = vec![0u8; (len - tail_start).min(65536) as usize];
            if let Ok(n) = f.read(&mut tail) {
                h = fnv1a_bytes(&tail[..n], h);
            }
        }
    }
    format!("{:016x}", h)
}

fn fnv1a_bytes(bytes: &[u8], mut hash: u64) -> u64 {
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn sanitize_key(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

pub fn render_cache_path(
    file_hash: &str,
    revision: u64,
    page: usize,
    zoom_bucket: u32,
    rotation: i32,
    is_thumb: bool,
) -> PathBuf {
    let sub = if is_thumb { "thumbs" } else { "renders" };
    let fname = format!(
        "{}_r{}_p{}_z{}_rot{}.png",
        sanitize_key(file_hash),
        revision,
        page,
        zoom_bucket,
        ((rotation % 360) + 360) % 360
    );
    get_cache_root().join(sub).join(fname)
}

pub fn read_cached_render(
    file_hash: &str,
    revision: u64,
    page: usize,
    zoom_bucket: u32,
    rotation: i32,
    is_thumb: bool,
) -> Option<Vec<u8>> {
    let p = render_cache_path(file_hash, revision, page, zoom_bucket, rotation, is_thumb);
    let data = fs::read(&p).ok()?;
    if data.len() > 24 {
        Some(data)
    } else {
        None
    }
}

pub fn write_cached_render_async(
    file_hash: String,
    revision: u64,
    page: usize,
    zoom_bucket: u32,
    rotation: i32,
    is_thumb: bool,
    png_bytes: Vec<u8>,
) {
    if png_bytes.is_empty() || png_bytes.len() > 16 * 1024 * 1024 {
        return;
    }
    std::thread::spawn(move || {
        let p = render_cache_path(&file_hash, revision, page, zoom_bucket, rotation, is_thumb);
        let tmp = p.with_extension("tmp");
        if fs::write(&tmp, &png_bytes).is_ok() {
            let _ = fs::rename(&tmp, &p);
        }
        // Trigger occasional eviction (~1 in 16 writes) to avoid scanning directory on every write
        static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);
        if WRITE_COUNTER.fetch_add(1, Ordering::Relaxed) % 16 == 15 {
            enforce_disk_cache_limit();
        }
    });
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskCacheStats {
    pub cache_dir: String,
    pub total_bytes: u64,
    pub total_mb: f64,
    pub limit_mb: u32,
    pub render_files: usize,
    pub thumb_files: usize,
    pub index_files: usize,
}

fn collect_cache_entries(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) -> usize {
    let mut cnt = 0usize;
    if let Ok(rd) = fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_file() {
                if let Ok(m) = ent.metadata() {
                    let mt = m.modified().unwrap_or(UNIX_EPOCH);
                    out.push((p, m.len(), mt));
                    cnt += 1;
                }
            }
        }
    }
    cnt
}

pub fn get_disk_cache_stats() -> DiskCacheStats {
    let root = get_cache_root();
    let mut all = Vec::new();
    let render_files = collect_cache_entries(&root.join("renders"), &mut all);
    let thumb_files = collect_cache_entries(&root.join("thumbs"), &mut all);
    let index_files = collect_cache_entries(&root.join("search_index"), &mut all);
    let total_bytes: u64 = all.iter().map(|e| e.1).sum();
    DiskCacheStats {
        cache_dir: root.to_string_lossy().to_string(),
        total_bytes,
        total_mb: ((total_bytes as f64 / (1024.0 * 1024.0)) * 10.0).round() / 10.0,
        limit_mb: get_disk_cache_limit_mb(),
        render_files,
        thumb_files,
        index_files,
    }
}

pub fn clear_disk_cache() -> DiskCacheStats {
    let root = get_cache_root();
    for sub in ["renders", "thumbs", "search_index"] {
        let dir = root.join(sub);
        if let Ok(rd) = fs::read_dir(&dir) {
            for ent in rd.flatten() {
                let _ = fs::remove_file(ent.path());
            }
        }
    }
    if let Some(mem) = SEARCH_INDEX_MEM.get() {
        let mut g = mem.lock().unwrap();
        g.entries.clear();
        g.order.clear();
    }
    get_disk_cache_stats()
}

pub fn enforce_disk_cache_limit() {
    let limit = CACHE_LIMIT_BYTES.load(Ordering::Relaxed);
    let root = get_cache_root();
    let mut all = Vec::new();
    collect_cache_entries(&root.join("renders"), &mut all);
    collect_cache_entries(&root.join("thumbs"), &mut all);
    collect_cache_entries(&root.join("search_index"), &mut all);
    let mut total: u64 = all.iter().map(|e| e.1).sum();
    if total <= limit {
        return;
    }
    // Evict oldest modified files first until under 85% of limit
    all.sort_by_key(|e| e.2);
    let target = (limit * 85) / 100;
    for (p, sz, _) in all {
        if total <= target {
            break;
        }
        if fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(sz);
        }
    }
}

// ============================================================================
//  Persistent Per-File Text Search Index (RAM LRU + Compressed Disk Cache)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedTextIndex {
    file_hash: String,
    revision: u64,
    num_pages: usize,
    pages_text: Vec<String>,
}

struct SearchIndexMem {
    entries: HashMap<String, Arc<CachedTextIndex>>,
    order: VecDeque<String>,
}

static SEARCH_INDEX_MEM: OnceLock<Mutex<SearchIndexMem>> = OnceLock::new();

fn search_mem() -> &'static Mutex<SearchIndexMem> {
    SEARCH_INDEX_MEM.get_or_init(|| {
        Mutex::new(SearchIndexMem {
            entries: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

fn search_index_path(file_hash: &str, revision: u64) -> PathBuf {
    get_cache_root()
        .join("search_index")
        .join(format!("{}_r{}.zidx", sanitize_key(file_hash), revision))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSearchHit {
    pub page_number: usize,
    pub match_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSearchResponse {
    pub ok: bool,
    pub hits: Vec<IndexedSearchHit>,
    pub total_pages: usize,
    pub cache_hit: bool,
    pub elapsed_ms: f64,
}

pub fn search_file_indexed(
    ws: &crate::pdf_layout::PdfWorkspaceEngine,
    file_path: &str,
    query: &str,
    case_sensitive: bool,
) -> Result<IndexedSearchResponse, String> {
    let t0 = Instant::now();
    let term = query.trim();
    if term.is_empty() {
        return Ok(IndexedSearchResponse {
            ok: true,
            hits: Vec::new(),
            total_pages: 0,
            cache_hit: true,
            elapsed_ms: 0.0,
        });
    }

    let (_, rev, dirty) = ws.load_backing(file_path)?;
    let file_hash = compute_file_hash(file_path);
    let mem_key = format!("{}|{}|{}", file_hash, rev, if dirty { 1 } else { 0 });

    // 1. Check in-RAM hot LRU cache first (<1ms repeat searches!)
    let mut cache_hit = false;
    let index_arc = {
        let mut g = search_mem().lock().unwrap();
        if let Some(existing) = g.entries.get(&mem_key).cloned() {
            cache_hit = true;
            g.order.retain(|k| k != &mem_key);
            g.order.push_back(mem_key.clone());
            Some(existing)
        } else {
            None
        }
    };

    // 2. Check compressed disk index in %LOCALAPPDATA%\PDFForge\cache\search_index\
    let index_arc = match index_arc {
        Some(arc) => arc,
        None => {
            let disk_path = search_index_path(&file_hash, rev);
            let loaded_from_disk = if !dirty && disk_path.is_file() {
                fs::read(&disk_path).ok().and_then(|compressed| {
                    let mut dec = ZlibDecoder::new(&compressed[..]);
                    let mut raw = Vec::new();
                    dec.read_to_end(&mut raw).ok()?;
                    serde_json::from_slice::<CachedTextIndex>(&raw).ok()
                })
            } else {
                None
            };

            let built = if let Some(idx) = loaded_from_disk {
                cache_hit = true;
                Arc::new(idx)
            } else {
                // Build text index cleanly using Rust PDFium (native file read when unedited!)
                let pages_text = ws.extract_all_pages_text_fast(file_path)?;
                let idx = CachedTextIndex {
                    file_hash: file_hash.clone(),
                    revision: rev,
                    num_pages: pages_text.len(),
                    pages_text,
                };
                if !dirty {
                    if let Ok(json_bytes) = serde_json::to_vec(&idx) {
                        let dp = disk_path.clone();
                        std::thread::spawn(move || {
                            let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
                            if enc.write_all(&json_bytes).is_ok() {
                                if let Ok(out) = enc.finish() {
                                    let tmp = dp.with_extension("tmp");
                                    if fs::write(&tmp, &out).is_ok() {
                                        let _ = fs::rename(&tmp, &dp);
                                    }
                                }
                            }
                        });
                    }
                }
                Arc::new(idx)
            };

            let mut g = search_mem().lock().unwrap();
            g.entries.insert(mem_key.clone(), built.clone());
            g.order.push_back(mem_key.clone());
            while g.order.len() > 6 {
                if let Some(old) = g.order.pop_front() {
                    g.entries.remove(&old);
                }
            }
            built
        }
    };

    // 3. Fast substring scan across all pages
    let needle = if case_sensitive {
        term.to_string()
    } else {
        term.to_lowercase()
    };
    let mut hits = Vec::new();
    for (i, text) in index_arc.pages_text.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let count = if case_sensitive {
            text.matches(&needle).count()
        } else {
            text.to_lowercase().matches(&needle).count()
        };
        if count > 0 {
            hits.push(IndexedSearchHit {
                page_number: i + 1,
                match_count: count,
            });
        }
    }

    let elapsed_ms = (t0.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
    crate::perf::record_span(
        "search/indexed",
        elapsed_ms,
        format!("{} pages hit={} cache={}", index_arc.num_pages, hits.len(), cache_hit),
    );

    Ok(IndexedSearchResponse {
        ok: true,
        hits,
        total_pages: index_arc.num_pages,
        cache_hit,
        elapsed_ms,
    })
}
