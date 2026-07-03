use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use memmap2::{MmapMut, MmapOptions};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::types::Row;

pub const DEFAULT_ROWS_PER_PAGE: usize = 16;
const PAGE_SLOT_BYTES: usize = 65536;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PageBody {
    rows: HashMap<u64, Row>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageFileHeader {
    next_id: u64,
    page_count: u32,
}

struct LruCache {
    max_pages: usize,
    pages: HashMap<u32, PageBody>,
    order: VecDeque<u32>,
}

impl LruCache {
    fn new(max_pages: usize) -> Self {
        Self {
            max_pages,
            pages: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn len(&self) -> usize {
        self.pages.len()
    }

    fn get(&mut self, page_id: u32) -> Option<PageBody> {
        if !self.pages.contains_key(&page_id) {
            return None;
        }
        self.touch(page_id);
        self.pages.get(&page_id).cloned()
    }

    fn insert(&mut self, page_id: u32, body: PageBody) {
        if self.pages.contains_key(&page_id) {
            self.pages.insert(page_id, body);
            self.touch(page_id);
            return;
        }
        while self.pages.len() >= self.max_pages && !self.order.is_empty() {
            if let Some(evicted) = self.order.pop_front() {
                self.pages.remove(&evicted);
            }
        }
        self.pages.insert(page_id, body);
        self.touch(page_id);
    }

    fn touch(&mut self, page_id: u32) {
        if let Some(pos) = self.order.iter().position(|p| *p == page_id) {
            self.order.remove(pos);
        }
        self.order.push_back(page_id);
    }
}

/// Page-backed table storage with mmap persistence and LRU page cache.
pub struct PageTable {
    path: PathBuf,
    rows_per_page: usize,
    max_pages: usize,
    next_id: u64,
    page_count: u32,
    cache: RwLock<LruCache>,
    mmap: RwLock<Option<MmapMut>>,
}

impl PageTable {
    pub fn open(
        data_dir: &Path,
        table: &str,
        max_pages: usize,
        rows_per_page: usize,
    ) -> crate::error::Result<Self> {
        let path = data_dir.join(format!("{table}.pages"));
        let (next_id, page_count) = if path.exists() {
            let header = Self::read_header(&path)?;
            (header.next_id, header.page_count)
        } else {
            (1, 0)
        };

        let pt = Self {
            path,
            rows_per_page,
            max_pages,
            next_id,
            page_count,
            cache: RwLock::new(LruCache::new(max_pages)),
            mmap: RwLock::new(None),
        };
        pt.ensure_mmap()?;
        Ok(pt)
    }

    pub fn import_rows(
        &mut self,
        next_id: u64,
        rows: HashMap<u64, Row>,
    ) -> crate::error::Result<()> {
        self.next_id = next_id;
        for (row_id, row) in rows {
            self.put_row(row_id, row)?;
        }
        self.write_header()?;
        Ok(())
    }

    pub fn cache_len(&self) -> usize {
        self.cache.read().len()
    }

    pub fn row_count(&self) -> u64 {
        let mut count = 0u64;
        for page_id in 0..self.page_count {
            if let Ok(page) = self.load_page(page_id) {
                count += page.rows.len() as u64;
            }
        }
        count
    }

    fn page_id(row_id: u64, rows_per_page: usize) -> u32 {
        ((row_id - 1) / rows_per_page as u64) as u32
    }

    fn ensure_mmap(&self) -> crate::error::Result<()> {
        let mut guard = self.mmap.write();
        if guard.is_some() {
            return Ok(());
        }
        let min_size = Self::file_size_for_pages(self.page_count.max(1));
        if !self.path.exists() {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.path)?;
            file.set_len(min_size as u64)?;
        } else {
            let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
            if file.metadata()?.len() < min_size as u64 {
                file.set_len(min_size as u64)?;
            }
        }
        let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let mmap = unsafe { MmapOptions::new().map_mut(&file)? };
        *guard = Some(mmap);
        Ok(())
    }

    fn file_size_for_pages(page_count: u32) -> usize {
        std::mem::size_of::<PageFileHeader>() + PAGE_SLOT_BYTES * page_count.max(1) as usize
    }

    fn read_header(path: &Path) -> crate::error::Result<PageFileHeader> {
        let file = OpenOptions::new().read(true).open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        if mmap.len() < std::mem::size_of::<PageFileHeader>() {
            return Ok(PageFileHeader {
                next_id: 1,
                page_count: 0,
            });
        }
        bincode::deserialize(&mmap[..std::mem::size_of::<PageFileHeader>()])
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))
    }

    fn write_header(&self) -> crate::error::Result<()> {
        self.ensure_mmap()?;
        let header = PageFileHeader {
            next_id: self.next_id,
            page_count: self.page_count,
        };
        let encoded = bincode::serialize(&header)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;
        let mut mmap = self.mmap.write();
        let map = mmap.as_mut().unwrap();
        map[..encoded.len()].copy_from_slice(&encoded);
        Ok(())
    }

    fn load_page(&self, page_id: u32) -> crate::error::Result<PageBody> {
        if let Some(body) = self.cache.write().get(page_id) {
            return Ok(body);
        }
        self.ensure_mmap()?;
        let offset = std::mem::size_of::<PageFileHeader>() + PAGE_SLOT_BYTES * page_id as usize;
        let mmap = self.mmap.read();
        let map = mmap.as_ref().unwrap();
        if offset + 4 > map.len() {
            return Ok(PageBody::default());
        }
        let len = u32::from_le_bytes(map[offset..offset + 4].try_into().unwrap()) as usize;
        if len == 0 || offset + 4 + len > map.len() {
            return Ok(PageBody::default());
        }
        let body: PageBody = bincode::deserialize(&map[offset + 4..offset + 4 + len])
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;
        self.cache.write().insert(page_id, body.clone());
        Ok(body)
    }

    fn store_page(&self, page_id: u32, body: &PageBody) -> crate::error::Result<()> {
        if page_id >= self.page_count {
            return Err(crate::error::MoodengError::Storage(format!(
                "page {page_id} out of range"
            )));
        }
        self.ensure_mmap()?;
        let encoded = bincode::serialize(body)
            .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;
        if encoded.len() + 4 > PAGE_SLOT_BYTES {
            return Err(crate::error::MoodengError::Storage("page overflow".into()));
        }
        let offset = std::mem::size_of::<PageFileHeader>() + PAGE_SLOT_BYTES * page_id as usize;
        let mut mmap = self.mmap.write();
        let map = mmap.as_mut().unwrap();
        map[offset..offset + PAGE_SLOT_BYTES].fill(0);
        map[offset..offset + 4].copy_from_slice(&(encoded.len() as u32).to_le_bytes());
        map[offset + 4..offset + 4 + encoded.len()].copy_from_slice(&encoded);
        self.cache.write().insert(page_id, body.clone());
        Ok(())
    }

    fn grow_if_needed(&mut self, page_id: u32) -> crate::error::Result<()> {
        if page_id < self.page_count {
            return Ok(());
        }
        let new_count = page_id + 1;
        let new_size = Self::file_size_for_pages(new_count);
        {
            let mut mmap = self.mmap.write();
            if let Some(map) = mmap.as_ref() {
                map.flush()
                    .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;
            }
            *mmap = None;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&self.path)?;
        file.set_len(new_size as u64)?;
        self.page_count = new_count;
        self.ensure_mmap()?;
        self.write_header()?;
        Ok(())
    }

    fn put_row(&mut self, row_id: u64, row: Row) -> crate::error::Result<()> {
        let page_id = Self::page_id(row_id, self.rows_per_page);
        self.grow_if_needed(page_id)?;
        let mut body = self.load_page(page_id)?;
        body.rows.insert(row_id, row);
        self.store_page(page_id, &body)?;
        Ok(())
    }

    pub fn insert(&mut self, row: Row) -> crate::error::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.put_row(id, row)?;
        self.write_header()?;
        Ok(id)
    }

    pub fn apply_insert(&mut self, row_id: u64, row: Row) -> crate::error::Result<()> {
        if row_id >= self.next_id {
            self.next_id = row_id + 1;
        }
        self.put_row(row_id, row)?;
        self.write_header()?;
        Ok(())
    }

    pub fn get(&self, row_id: u64) -> crate::error::Result<Option<Row>> {
        let page_id = Self::page_id(row_id, self.rows_per_page);
        if page_id >= self.page_count {
            return Ok(None);
        }
        let body = self.load_page(page_id)?;
        Ok(body.rows.get(&row_id).cloned())
    }

    pub fn scan(&self) -> crate::error::Result<Vec<(u64, Row)>> {
        let mut rows = Vec::new();
        for page_id in 0..self.page_count {
            let body = self.load_page(page_id)?;
            for (id, row) in body.rows {
                rows.push((id, row));
            }
        }
        rows.sort_by_key(|(id, _)| *id);
        Ok(rows)
    }

    pub fn update(&mut self, row_id: u64, row: Row) -> crate::error::Result<bool> {
        let page_id = Self::page_id(row_id, self.rows_per_page);
        if page_id >= self.page_count {
            return Ok(false);
        }
        let mut body = self.load_page(page_id)?;
        if body.rows.insert(row_id, row).is_some() {
            self.store_page(page_id, &body)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn delete(&mut self, row_id: u64) -> crate::error::Result<bool> {
        let page_id = Self::page_id(row_id, self.rows_per_page);
        if page_id >= self.page_count {
            return Ok(false);
        }
        let mut body = self.load_page(page_id)?;
        if body.rows.remove(&row_id).is_some() {
            self.store_page(page_id, &body)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn flush_all(&self) -> crate::error::Result<()> {
        self.write_header()?;
        if let Some(mmap) = self.mmap.read().as_ref() {
            mmap.flush()
                .map_err(|e| crate::error::MoodengError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    pub fn on_disk_page_count(&self) -> u32 {
        self.page_count
    }

    pub fn remove_file(&self) -> crate::error::Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Row, Value};

    #[test]
    fn inserts_span_many_pages_with_small_cache() {
        let dir = std::env::temp_dir().join(format!("pt_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut pt = PageTable::open(&dir, "t", 2, 8).unwrap();
        for i in 1..=200i64 {
            let row = Row::new(vec![Value::Int4(i as i32)]);
            pt.insert(row)
                .unwrap_or_else(|e| panic!("insert {i} failed: {e}"));
        }
        assert_eq!(pt.scan().unwrap().len(), 200);
        assert_eq!(pt.row_count(), 200, "page_count={}", pt.on_disk_page_count());
        assert!(pt.cache_len() <= 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
