use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use image::{ImageFormat, imageops::FilterType};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

use crate::classify::{classify_text, compact_title};
use crate::config::RetentionMode;
use crate::model::{
    ClipDetail, ClipItem, ClipKind, ClipQuery, NewClipInput, SyncEvent, SyncOp,
};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid clip input: {0}")]
    InvalidInput(String),
}

pub struct ClipStore {
    data_dir: PathBuf,
    blobs_dir: PathBuf,
    conn: Connection,
}

impl ClipStore {
    pub fn open(data_dir: &Path) -> Result<Self, StorageError> {
        let blobs_dir = data_dir.join("blobs");
        let thumbs_dir = data_dir.join("thumbs");
        fs::create_dir_all(&blobs_dir)?;
        fs::create_dir_all(&thumbs_dir)?;

        let db_path = data_dir.join("klip.sqlite");
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self {
            data_dir: data_dir.to_path_buf(),
            blobs_dir,
            conn,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn blobs_dir(&self) -> &Path {
        &self.blobs_dir
    }

    fn migrate(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS clips (
                id TEXT PRIMARY KEY,
                content_hash TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                preview_text TEXT,
                text_content TEXT,
                color_css TEXT,
                source_app TEXT,
                source_device TEXT NOT NULL,
                char_count INTEGER,
                word_count INTEGER,
                image_width INTEGER,
                image_height INTEGER,
                byte_size INTEGER NOT NULL,
                copy_count INTEGER NOT NULL DEFAULT 1,
                pinned INTEGER NOT NULL DEFAULT 0,
                first_copied_at INTEGER NOT NULL,
                last_copied_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                deleted_at INTEGER,
                blob_path TEXT,
                thumb_path TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_clips_order
                ON clips (deleted_at, pinned DESC, last_copied_at DESC);
            CREATE INDEX IF NOT EXISTS idx_clips_kind
                ON clips (deleted_at, kind, pinned DESC, last_copied_at DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(
                clip_id UNINDEXED,
                title,
                text_content,
                source_app,
                tokenize = 'unicode61'
            );

            CREATE TABLE IF NOT EXISTS pending_events (
                event_id TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL,
                payload_json TEXT NOT NULL,
                blob_hash TEXT
            );

            CREATE TABLE IF NOT EXISTS seen_remote_events (
                object_key TEXT PRIMARY KEY,
                processed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sync_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn now_ms() -> i64 {
        jiff::Timestamp::now().as_millisecond()
    }

    pub fn capture_local(
        &mut self,
        input: NewClipInput,
        device_id: &str,
        device_name: &str,
        queue_sync: bool,
    ) -> Result<Option<ClipItem>, StorageError> {
        let now = input.copied_at_ms.unwrap_or_else(Self::now_ms);
        let (content_hash, kind, title, preview_text, text_content, color_css, char_count, word_count, image_width, image_height, byte_size, blob_path, thumb_path, blob_hash) =
            match input.kind {
                ClipKind::Image => {
                    let png = input
                        .png_bytes
                        .filter(|b| !b.is_empty())
                        .ok_or_else(|| StorageError::InvalidInput("empty image bytes".into()))?;
                    let hash = sha256_hex(&png);
                    let (w, h, b_rel, t_rel) = self.store_image_blob(&hash, &png)?;
                    let title = format!("Image ({w}×{h})");
                    (
                        hash.clone(),
                        ClipKind::Image,
                        title,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(w),
                        Some(h),
                        png.len() as u64,
                        Some(b_rel),
                        Some(t_rel),
                        Some(hash),
                    )
                }
                _ => {
                    let raw = input
                        .text
                        .filter(|t| !t.trim().is_empty())
                        .ok_or_else(|| StorageError::InvalidInput("empty text".into()))?;
                    let hash = sha256_hex(raw.as_bytes());
                    let classified = classify_text(&raw);
                    let byte_size = raw.len() as u64;
                    (
                        hash,
                        classified.kind,
                        if classified.title.is_empty() {
                            compact_title(&raw, 120)
                        } else {
                            classified.title
                        },
                        Some(classified.preview_text),
                        Some(raw),
                        classified.color_css,
                        Some(classified.char_count),
                        Some(classified.word_count),
                        None,
                        None,
                        byte_size,
                        None,
                        None,
                        None,
                    )
                }
            };

        let item = self.upsert_record(UpsertParams {
            content_hash: &content_hash,
            kind,
            title: &title,
            preview_text: preview_text.as_deref(),
            text_content: text_content.as_deref(),
            color_css: color_css.as_deref(),
            source_app: input.source_app.as_deref(),
            source_device: device_name,
            char_count,
            word_count,
            image_width,
            image_height,
            byte_size,
            copy_count_delta: input.copy_count.unwrap_or(1),
            pinned: input.pinned,
            first_copied_at: now,
            last_copied_at: now,
            updated_at: now,
            blob_path: blob_path.as_deref(),
            thumb_path: thumb_path.as_deref(),
            is_remote: false,
        })?;

        if queue_sync {
            let evt = SyncEvent {
                event_id: Ulid::new().to_string(),
                device_id: device_id.to_string(),
                device_name: device_name.to_string(),
                created_at: now,
                op: SyncOp::Upsert {
                    content_hash: item.content_hash.clone(),
                    kind: item.kind,
                    text: text_content,
                    blob_hash: blob_hash.clone(),
                    image_width: item.image_width,
                    image_height: item.image_height,
                    byte_size: item.byte_size,
                    source_app: item.source_app.clone(),
                    first_copied_at: item.first_copied_at,
                    last_copied_at: item.last_copied_at,
                    copy_count: item.copy_count,
                    pinned: item.pinned,
                },
            };
            self.enqueue_event(&evt, blob_hash.as_deref())?;
        }

        Ok(Some(item))
    }

    pub fn apply_remote_event(
        &mut self,
        event: &SyncEvent,
        blob_png: Option<&[u8]>,
    ) -> Result<Option<ClipItem>, StorageError> {
        match &event.op {
            SyncOp::Upsert {
                content_hash,
                kind,
                text,
                blob_hash,
                image_width,
                image_height,
                byte_size,
                source_app,
                first_copied_at,
                last_copied_at,
                copy_count,
                pinned,
            } => {
                // Respect newer local tombstone if item was deleted later than this event.
                let deleted_at: Option<i64> = self
                    .conn
                    .query_row(
                        "SELECT deleted_at FROM clips WHERE content_hash = ?1",
                        params![content_hash],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(del_ts) = deleted_at
                    && del_ts >= *last_copied_at
                {
                    return Ok(None);
                }

                let (title, preview_text, color_css, char_count, word_count, blob_path, thumb_path, w, h) =
                    match kind {
                        ClipKind::Image => {
                            let (w, h, b_rel, t_rel) = if let Some(png) = blob_png {
                                let hash = blob_hash.as_deref().unwrap_or(content_hash);
                                self.store_image_blob(hash, png)?
                            } else {
                                (
                                    image_width.unwrap_or(0),
                                    image_height.unwrap_or(0),
                                    String::new(),
                                    String::new(),
                                )
                            };
                            (
                                format!("Image ({w}×{h})"),
                                None,
                                None,
                                None,
                                None,
                                if b_rel.is_empty() { None } else { Some(b_rel) },
                                if t_rel.is_empty() { None } else { Some(t_rel) },
                                Some(w),
                                Some(h),
                            )
                        }
                        _ => {
                            let raw = text.as_deref().unwrap_or("");
                            let c = classify_text(raw);
                            (
                                c.title,
                                Some(c.preview_text),
                                c.color_css,
                                Some(c.char_count),
                                Some(c.word_count),
                                None,
                                None,
                                None,
                                None,
                            )
                        }
                    };

                let item = self.upsert_record(UpsertParams {
                    content_hash,
                    kind: *kind,
                    title: &title,
                    preview_text: preview_text.as_deref(),
                    text_content: text.as_deref(),
                    color_css: color_css.as_deref(),
                    source_app: source_app.as_deref(),
                    source_device: &event.device_name,
                    char_count,
                    word_count,
                    image_width: w,
                    image_height: h,
                    byte_size: *byte_size,
                    copy_count_delta: *copy_count,
                    pinned: *pinned,
                    first_copied_at: *first_copied_at,
                    last_copied_at: *last_copied_at,
                    updated_at: event.created_at,
                    blob_path: blob_path.as_deref(),
                    thumb_path: thumb_path.as_deref(),
                    is_remote: true,
                })?;
                Ok(Some(item))
            }
            SyncOp::SetPin {
                content_hash,
                pinned,
                updated_at,
            } => {
                self.conn.execute(
                    "UPDATE clips SET pinned = ?2, updated_at = ?3 WHERE content_hash = ?1 AND updated_at <= ?3",
                    params![content_hash, *pinned as i64, updated_at],
                )?;
                Ok(None)
            }
            SyncOp::Delete {
                content_hash,
                deleted_at,
            } => {
                self.conn.execute(
                    "UPDATE clips SET deleted_at = ?2, updated_at = ?2 WHERE content_hash = ?1 AND updated_at <= ?2",
                    params![content_hash, deleted_at],
                )?;
                Ok(None)
            }
        }
    }

    pub fn query_items(&self, query: &ClipQuery) -> Result<Vec<ClipItem>, StorageError> {
        let limit = query.limit.unwrap_or(100).min(500) as i64;
        let offset = query.offset.unwrap_or(0) as i64;
        let search = query.search.as_deref().unwrap_or("").trim();

        let mut sql = String::from(
            r#"
            SELECT c.id, c.content_hash, c.kind, c.title, c.preview_text, c.color_css,
                   c.source_app, c.source_device, c.char_count, c.word_count,
                   c.image_width, c.image_height, c.byte_size, c.copy_count,
                   c.pinned, c.first_copied_at, c.last_copied_at, c.updated_at,
                   c.deleted_at, c.thumb_path, c.blob_path
            FROM clips c
            "#,
        );

        let fts_match = build_fts_query(search);
        if fts_match.is_some() {
            sql.push_str(" JOIN clips_fts f ON f.clip_id = c.id ");
        }

        sql.push_str(" WHERE c.deleted_at IS NULL ");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(m) = fts_match {
            sql.push_str(" AND clips_fts MATCH ? ");
            args.push(m.into());
        }
        if let Some(kind) = query.kind {
            sql.push_str(" AND c.kind = ? ");
            args.push(kind.as_str().to_string().into());
        }
        if query.pinned_only {
            sql.push_str(" AND c.pinned = 1 ");
        }

        sql.push_str(" ORDER BY c.pinned DESC, c.last_copied_at DESC LIMIT ? OFFSET ? ");
        args.push(limit.into());
        args.push(offset.into());

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), row_to_clip_item)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn get_detail(&self, id: &str) -> Result<Option<ClipDetail>, StorageError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, content_hash, kind, title, preview_text, color_css,
                   source_app, source_device, char_count, word_count,
                   image_width, image_height, byte_size, copy_count,
                   pinned, first_copied_at, last_copied_at, updated_at,
                   deleted_at, thumb_path, blob_path, text_content
            FROM clips WHERE id = ?1 AND deleted_at IS NULL
            "#,
        )?;
        let row = stmt
            .query_row(params![id], |r| {
                let item = row_to_clip_item(r)?;
                let text_content: Option<String> = r.get(21)?;
                Ok((item, text_content))
            })
            .optional()?;

        let Some((item, text_content)) = row else {
            return Ok(None);
        };

        let image_data_url = if item.kind == ClipKind::Image {
            item.blob_path.as_ref().and_then(|rel| {
                let path = self.data_dir.join(rel);
                fs::read(path)
                    .ok()
                    .map(|bytes| format!("data:image/png;base64,{}", B64.encode(bytes)))
            })
        } else {
            None
        };

        Ok(Some(ClipDetail {
            item,
            text_content,
            image_data_url,
        }))
    }

    pub fn read_image_bytes(&self, item: &ClipItem) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(rel) = item.blob_path.as_deref() else {
            return Ok(None);
        };
        let path = self.data_dir.join(rel);
        if path.exists() {
            Ok(Some(fs::read(path)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<(ClipItem, Option<String>)>, StorageError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, content_hash, kind, title, preview_text, color_css,
                   source_app, source_device, char_count, word_count,
                   image_width, image_height, byte_size, copy_count,
                   pinned, first_copied_at, last_copied_at, updated_at,
                   deleted_at, thumb_path, blob_path, text_content
            FROM clips WHERE id = ?1 AND deleted_at IS NULL
            "#,
        )?;
        let res = stmt
            .query_row(params![id], |r| {
                let item = row_to_clip_item(r)?;
                let text_content: Option<String> = r.get(21)?;
                Ok((item, text_content))
            })
            .optional()?;
        Ok(res)
    }

    pub fn toggle_pin(
        &mut self,
        id: &str,
        device_id: &str,
        device_name: &str,
        queue_sync: bool,
    ) -> Result<Option<bool>, StorageError> {
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT content_hash, pinned FROM clips WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((content_hash, cur_pin)) = row else {
            return Ok(None);
        };
        let new_pin = cur_pin == 0;
        let now = Self::now_ms();
        self.conn.execute(
            "UPDATE clips SET pinned = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, new_pin as i64, now],
        )?;
        if queue_sync {
            let evt = SyncEvent {
                event_id: Ulid::new().to_string(),
                device_id: device_id.to_string(),
                device_name: device_name.to_string(),
                created_at: now,
                op: SyncOp::SetPin {
                    content_hash,
                    pinned: new_pin,
                    updated_at: now,
                },
            };
            self.enqueue_event(&evt, None)?;
        }
        Ok(Some(new_pin))
    }

    pub fn delete_item(
        &mut self,
        id: &str,
        device_id: &str,
        device_name: &str,
        queue_sync: bool,
    ) -> Result<bool, StorageError> {
        let content_hash: Option<String> = self
            .conn
            .query_row(
                "SELECT content_hash FROM clips WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(content_hash) = content_hash else {
            return Ok(false);
        };
        let now = Self::now_ms();
        self.conn.execute(
            "UPDATE clips SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        self.conn
            .execute("DELETE FROM clips_fts WHERE clip_id = ?1", params![id])?;

        if queue_sync {
            let evt = SyncEvent {
                event_id: Ulid::new().to_string(),
                device_id: device_id.to_string(),
                device_name: device_name.to_string(),
                created_at: now,
                op: SyncOp::Delete {
                    content_hash,
                    deleted_at: now,
                },
            };
            self.enqueue_event(&evt, None)?;
        }
        Ok(true)
    }

    pub fn clear_unpinned(
        &mut self,
        device_id: &str,
        device_name: &str,
        queue_sync: bool,
    ) -> Result<usize, StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, content_hash FROM clips WHERE deleted_at IS NULL AND pinned = 0")?;
        let targets: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        let now = Self::now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE clips SET deleted_at = ?1, updated_at = ?1 WHERE deleted_at IS NULL AND pinned = 0",
            params![now],
        )?;
        tx.execute(
            "DELETE FROM clips_fts WHERE clip_id IN (SELECT id FROM clips WHERE deleted_at IS NOT NULL)",
            [],
        )?;
        tx.commit()?;

        if queue_sync {
            for (_, content_hash) in &targets {
                let evt = SyncEvent {
                    event_id: Ulid::new().to_string(),
                    device_id: device_id.to_string(),
                    device_name: device_name.to_string(),
                    created_at: now,
                    op: SyncOp::Delete {
                        content_hash: content_hash.clone(),
                        deleted_at: now,
                    },
                };
                self.enqueue_event(&evt, None)?;
            }
        }
        Ok(targets.len())
    }

    pub fn prune(
        &mut self,
        mode: RetentionMode,
        max_items: usize,
        max_days: u32,
    ) -> Result<usize, StorageError> {
        self.prune_at(mode, max_items, max_days, Self::now_ms())
    }

    pub fn prune_at(
        &mut self,
        mode: RetentionMode,
        max_items: usize,
        max_days: u32,
        now_ms: i64,
    ) -> Result<usize, StorageError> {
        let mut total_removed = 0usize;

        if matches!(mode, RetentionMode::Days | RetentionMode::Both) && max_days > 0 {
            let cutoff = now_ms - i64::from(max_days) * 86_400_000;
            let removed = self.conn.execute(
                r#"
                UPDATE clips SET deleted_at = ?1, updated_at = ?1
                WHERE deleted_at IS NULL AND pinned = 0 AND last_copied_at < ?2
                "#,
                params![now_ms, cutoff],
            )?;
            total_removed += removed;
        }

        if matches!(mode, RetentionMode::Count | RetentionMode::Both) && max_items > 0 {
            let count: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM clips WHERE deleted_at IS NULL AND pinned = 0",
                [],
                |r| r.get(0),
            )?;
            if count as usize > max_items {
                let excess = (count as usize - max_items) as i64;
                let removed = self.conn.execute(
                    r#"
                    UPDATE clips SET deleted_at = ?1, updated_at = ?1
                    WHERE id IN (
                        SELECT id FROM clips
                        WHERE deleted_at IS NULL AND pinned = 0
                        ORDER BY last_copied_at ASC
                        LIMIT ?2
                    )
                    "#,
                    params![now_ms, excess],
                )?;
                total_removed += removed;
            }
        }

        if total_removed > 0 {
            self.conn.execute(
                "DELETE FROM clips_fts WHERE clip_id IN (SELECT id FROM clips WHERE deleted_at IS NOT NULL)",
                [],
            )?;
            self.cleanup_orphan_blobs()?;
        }

        Ok(total_removed)
    }

    pub fn prune_to_limit(&mut self, max_items: usize) -> Result<usize, StorageError> {
        self.prune(RetentionMode::Count, max_items, 0)
    }

    fn cleanup_orphan_blobs(&self) -> Result<(), StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT blob_path FROM clips WHERE deleted_at IS NULL AND blob_path IS NOT NULL")?;
        let live: std::collections::HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;

        for folder in ["blobs", "thumbs"] {
            let dir = self.data_dir.join(folder);
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        let blob_rel = format!("blobs/{name}");
                        if !live.contains(&blob_rel) {
                            let _ = fs::remove_file(path);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn pending_events(&self) -> Result<Vec<(String, String, Option<String>)>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, payload_json, blob_hash FROM pending_events ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn mark_event_sent(&self, event_id: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM pending_events WHERE event_id = ?1",
            params![event_id],
        )?;
        Ok(())
    }

    pub fn is_remote_event_seen(&self, object_key: &str) -> Result<bool, StorageError> {
        let found: Option<String> = self
            .conn
            .query_row(
                "SELECT object_key FROM seen_remote_events WHERE object_key = ?1",
                params![object_key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    pub fn mark_remote_event_seen(&self, object_key: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO seen_remote_events (object_key, processed_at) VALUES (?1, ?2)",
            params![object_key, Self::now_ms()],
        )?;
        Ok(())
    }

    pub fn has_blob(&self, blob_hash: &str) -> bool {
        self.blobs_dir.join(format!("{blob_hash}.png")).exists()
    }

    pub fn read_blob(&self, blob_hash: &str) -> Result<Vec<u8>, StorageError> {
        Ok(fs::read(self.blobs_dir.join(format!("{blob_hash}.png")))?)
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO sync_state (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn stats(&self) -> Result<(usize, usize), StorageError> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM clips WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        let pending: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM pending_events", [], |r| r.get(0))?;
        Ok((total as usize, pending as usize))
    }

    fn enqueue_event(&self, evt: &SyncEvent, blob_hash: Option<&str>) -> Result<(), StorageError> {
        let json = serde_json::to_string(evt)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO pending_events (event_id, created_at, payload_json, blob_hash) VALUES (?1, ?2, ?3, ?4)",
            params![evt.event_id, evt.created_at, json, blob_hash],
        )?;
        Ok(())
    }

    fn store_image_blob(
        &self,
        hash: &str,
        png_bytes: &[u8],
    ) -> Result<(u32, u32, String, String), StorageError> {
        let blob_rel = format!("blobs/{hash}.png");
        let thumb_rel = format!("thumbs/{hash}.png");
        let blob_abs = self.data_dir.join(&blob_rel);
        let thumb_abs = self.data_dir.join(&thumb_rel);

        let img = image::load_from_memory(png_bytes)?;
        let (w, h) = (img.width(), img.height());

        if !blob_abs.exists() {
            fs::write(&blob_abs, png_bytes)?;
        }
        if !thumb_abs.exists() {
            let thumb = img.resize(320, 320, FilterType::Triangle);
            let mut buf = Cursor::new(Vec::new());
            thumb.write_to(&mut buf, ImageFormat::Png)?;
            fs::write(&thumb_abs, buf.into_inner())?;
        }
        Ok((w, h, blob_rel, thumb_rel))
    }

    fn upsert_record(&mut self, p: UpsertParams<'_>) -> Result<ClipItem, StorageError> {
        let existing: Option<(String, i64, i64, i64, i64, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT id, copy_count, pinned, first_copied_at, last_copied_at, deleted_at FROM clips WHERE content_hash = ?1",
                params![p.content_hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()?;

        let id = if let Some((existing_id, prev_count, prev_pin, prev_first, prev_last, _)) = existing
        {
            let new_count = if p.is_remote {
                prev_count.max(p.copy_count_delta as i64)
            } else {
                prev_count + p.copy_count_delta as i64
            };
            let new_pin = if p.is_remote {
                prev_pin != 0 || p.pinned
            } else {
                prev_pin != 0 || p.pinned
            };
            let new_first = prev_first.min(p.first_copied_at);
            let new_last = prev_last.max(p.last_copied_at);

            self.conn.execute(
                r#"
                UPDATE clips SET
                    kind = ?2,
                    title = ?3,
                    preview_text = ?4,
                    text_content = COALESCE(?5, text_content),
                    color_css = ?6,
                    source_app = COALESCE(?7, source_app),
                    source_device = ?8,
                    copy_count = ?9,
                    pinned = ?10,
                    first_copied_at = ?11,
                    last_copied_at = ?12,
                    updated_at = ?13,
                    deleted_at = NULL,
                    blob_path = COALESCE(?14, blob_path),
                    thumb_path = COALESCE(?15, thumb_path)
                WHERE id = ?1
                "#,
                params![
                    existing_id,
                    p.kind.as_str(),
                    p.title,
                    p.preview_text,
                    p.text_content,
                    p.color_css,
                    p.source_app,
                    p.source_device,
                    new_count,
                    new_pin as i64,
                    new_first,
                    new_last,
                    p.updated_at,
                    p.blob_path,
                    p.thumb_path,
                ],
            )?;
            self.conn
                .execute("DELETE FROM clips_fts WHERE clip_id = ?1", params![existing_id])?;
            self.conn.execute(
                "INSERT INTO clips_fts (clip_id, title, text_content, source_app) VALUES (?1, ?2, ?3, ?4)",
                params![
                    existing_id,
                    p.title,
                    p.text_content.unwrap_or(""),
                    p.source_app.unwrap_or("")
                ],
            )?;
            existing_id
        } else {
            let new_id = Ulid::new().to_string();
            self.conn.execute(
                r#"
                INSERT INTO clips (
                    id, content_hash, kind, title, preview_text, text_content, color_css,
                    source_app, source_device, char_count, word_count, image_width, image_height,
                    byte_size, copy_count, pinned, first_copied_at, last_copied_at, updated_at,
                    deleted_at, blob_path, thumb_path
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                    ?8, ?9, ?10, ?11, ?12, ?13,
                    ?14, ?15, ?16, ?17, ?18, ?19,
                    NULL, ?20, ?21
                )
                "#,
                params![
                    new_id,
                    p.content_hash,
                    p.kind.as_str(),
                    p.title,
                    p.preview_text,
                    p.text_content,
                    p.color_css,
                    p.source_app,
                    p.source_device,
                    p.char_count.map(|v| v as i64),
                    p.word_count.map(|v| v as i64),
                    p.image_width.map(|v| v as i64),
                    p.image_height.map(|v| v as i64),
                    p.byte_size as i64,
                    p.copy_count_delta as i64,
                    p.pinned as i64,
                    p.first_copied_at,
                    p.last_copied_at,
                    p.updated_at,
                    p.blob_path,
                    p.thumb_path,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO clips_fts (clip_id, title, text_content, source_app) VALUES (?1, ?2, ?3, ?4)",
                params![
                    new_id,
                    p.title,
                    p.text_content.unwrap_or(""),
                    p.source_app.unwrap_or("")
                ],
            )?;
            new_id
        };

        let (item, _) = self.get_by_id(&id)?.expect("upserted item must exist");
        Ok(item)
    }
}

struct UpsertParams<'a> {
    content_hash: &'a str,
    kind: ClipKind,
    title: &'a str,
    preview_text: Option<&'a str>,
    text_content: Option<&'a str>,
    color_css: Option<&'a str>,
    source_app: Option<&'a str>,
    source_device: &'a str,
    char_count: Option<u64>,
    word_count: Option<u64>,
    image_width: Option<u32>,
    image_height: Option<u32>,
    byte_size: u64,
    copy_count_delta: u64,
    pinned: bool,
    first_copied_at: i64,
    last_copied_at: i64,
    updated_at: i64,
    blob_path: Option<&'a str>,
    thumb_path: Option<&'a str>,
    is_remote: bool,
}

fn row_to_clip_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClipItem> {
    let kind_str: String = r.get(2)?;
    Ok(ClipItem {
        id: r.get(0)?,
        content_hash: r.get(1)?,
        kind: ClipKind::from_db(&kind_str),
        title: r.get(3)?,
        preview_text: r.get(4)?,
        color_css: r.get(5)?,
        source_app: r.get(6)?,
        source_device: r.get(7)?,
        char_count: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        word_count: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        image_width: r.get::<_, Option<i64>>(10)?.map(|v| v as u32),
        image_height: r.get::<_, Option<i64>>(11)?.map(|v| v as u32),
        byte_size: r.get::<_, i64>(12)? as u64,
        copy_count: r.get::<_, i64>(13)? as u64,
        pinned: r.get::<_, i64>(14)? != 0,
        first_copied_at: r.get(15)?,
        last_copied_at: r.get(16)?,
        updated_at: r.get(17)?,
        deleted_at: r.get(18)?,
        thumb_path: r.get(19)?,
        blob_path: r.get(20)?,
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Converts arbitrary user input into a safe FTS5 MATCH query using quoted prefix terms.
fn build_fts_query(raw: &str) -> Option<String> {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .filter_map(|part| {
            let cleaned: String = part
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("\"{cleaned}\"*"))
            }
        })
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klip-test-{label}-{}", Ulid::new()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stores_dedups_searches_and_applies_remote_events() {
        let dir = temp_dir("storage");
        let mut store = ClipStore::open(&dir).unwrap();

        let item1 = store
            .capture_local(
                NewClipInput {
                    kind: ClipKind::Text,
                    text: Some("https://developers.cloudflare.com/r2/".into()),
                    png_bytes: None,
                    source_app: Some("Zen".into()),
                    copied_at_ms: Some(1000),
                    pinned: false,
                    copy_count: None,
                },
                "dev1",
                "mac",
                true,
            )
            .unwrap()
            .unwrap();
        assert_eq!(item1.kind, ClipKind::Link);
        assert_eq!(item1.copy_count, 1);

        // Dedup same content bumps copy_count
        let item2 = store
            .capture_local(
                NewClipInput {
                    kind: ClipKind::Text,
                    text: Some("https://developers.cloudflare.com/r2/".into()),
                    png_bytes: None,
                    source_app: Some("Ghostty".into()),
                    copied_at_ms: Some(2000),
                    pinned: false,
                    copy_count: None,
                },
                "dev1",
                "mac",
                true,
            )
            .unwrap()
            .unwrap();
        assert_eq!(item1.id, item2.id);
        assert_eq!(item2.copy_count, 2);

        let hits = store
            .query_items(&ClipQuery {
                search: Some("cloudflare r2".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, item1.id);

        assert!(store.toggle_pin(&item1.id, "dev1", "mac", true).unwrap().unwrap());
        let cleared = store.clear_unpinned("dev1", "mac", true).unwrap();
        assert_eq!(cleared, 0);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn prunes_by_count_days_and_both() {
        let dir = temp_dir("retention");
        let mut store = ClipStore::open(&dir).unwrap();
        let day_ms = 86_400_000i64;
        let now = 100 * day_ms;

        // 3 unpinned items: ages 40d, 10d, 1d; plus 1 pinned at 60d
        for (idx, age_days, pinned) in [
            (1, 40, false),
            (2, 10, false),
            (3, 1, false),
            (4, 60, true),
        ] {
            store
                .capture_local(
                    NewClipInput {
                        kind: ClipKind::Text,
                        text: Some(format!("clip-{idx}")),
                        png_bytes: None,
                        source_app: None,
                        copied_at_ms: Some(now - age_days * day_ms),
                        pinned,
                        copy_count: None,
                    },
                    "dev1",
                    "mac",
                    false,
                )
                .unwrap();
        }

        // Unlimited keeps all 4
        assert_eq!(
            store.prune_at(RetentionMode::Unlimited, 1, 1, now).unwrap(),
            0
        );

        // Both(max_items=1, max_days=30):
        // - Days=30 deletes 40d (age 40 > 30), keeps 10d & 1d & pinned 60d
        // - Count=1 then trims oldest unpinned (10d), leaving 1d + pinned 60d
        let removed = store.prune_at(RetentionMode::Both, 1, 30, now).unwrap();
        assert_eq!(removed, 2);

        let remaining = store.query_items(&ClipQuery::default()).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().any(|i| i.pinned && i.title == "clip-4"));
        assert!(remaining.iter().any(|i| !i.pinned && i.title == "clip-3"));

        let _ = fs::remove_dir_all(dir);
    }
}
