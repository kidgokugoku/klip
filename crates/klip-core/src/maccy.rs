use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::ImageFormat;
use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

use crate::model::{ClipKind, MaccyImportReport, NewClipInput};
use crate::storage::{ClipStore, StorageError};

/// CoreData NSDate reference epoch (2001-01-01T00:00:00Z) offset from Unix epoch in seconds.
const COREDATA_EPOCH_OFFSET_SECS: f64 = 978_307_200.0;

#[derive(Debug, Error)]
pub enum MaccyError {
    #[error("maccy database not found at {0}")]
    NotFound(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

pub fn default_maccy_db_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Containers/org.p0deje.Maccy/Data/Library/Application Support/Maccy/Storage.sqlite"),
    )
}

pub fn import_from_maccy(
    store: &mut ClipStore,
    db_path: Option<&Path>,
    device_id: &str,
    device_name: &str,
    queue_sync: bool,
    max_image_bytes: usize,
) -> Result<MaccyImportReport, MaccyError> {
    let path = match db_path {
        Some(p) => p.to_path_buf(),
        None => default_maccy_db_path()
            .ok_or_else(|| MaccyError::NotFound("HOME not set".into()))?,
    };
    if !path.exists() {
        return Err(MaccyError::NotFound(path.display().to_string()));
    }

    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    let mut items_stmt = conn.prepare(
        r#"
        SELECT Z_PK, ZNUMBEROFCOPIES, ZLASTCOPIEDAT, ZAPPLICATION, ZPIN, ZTITLE
        FROM ZHISTORYITEM
        ORDER BY ZLASTCOPIEDAT ASC
        "#,
    )?;

    let rows: Vec<MaccyRow> = items_stmt
        .query_map([], |r| {
            let copies: Option<i64> = r.get(1)?;
            let last_ts: Option<f64> = r.get(2)?;
            let pin: Option<String> = r.get(4)?;
            let ms = last_ts
                .map(|s| ((s + COREDATA_EPOCH_OFFSET_SECS) * 1000.0) as i64)
                .unwrap_or_else(ClipStore::now_ms);
            Ok(MaccyRow {
                pk: r.get(0)?,
                copies: copies.unwrap_or(1).max(1),
                last_copied_ms: ms,
                app: r.get(3)?,
                pinned: pin.as_deref().is_some_and(|p| !p.is_empty()),
                title: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut content_stmt =
        conn.prepare("SELECT ZITEM, ZTYPE, ZVALUE FROM ZHISTORYITEMCONTENT")?;
    let mut contents_by_item: HashMap<i64, Vec<(String, Vec<u8>)>> = HashMap::new();
    let content_rows = content_stmt.query_map([], |r| {
        let item_id: Option<i64> = r.get(0)?;
        let ztype: Option<String> = r.get(1)?;
        let zval: Option<Vec<u8>> = r.get(2)?;
        Ok((item_id, ztype, zval))
    })?;

    for row in content_rows {
        let (Some(item_id), Some(ztype), Some(zval)) = row? else {
            continue;
        };
        contents_by_item
            .entry(item_id)
            .or_default()
            .push((ztype, zval));
    }

    let mut report = MaccyImportReport {
        imported: 0,
        skipped_duplicates: 0,
        skipped_empty_or_unsupported: 0,
        db_path: path.display().to_string(),
    };

    for row in rows {
        let entries = contents_by_item.remove(&row.pk).unwrap_or_default();
        if let Some(input) = build_input_from_maccy(&row, &entries, max_image_bytes) {
            let before = store.stats()?.0;
            match store.capture_local(input, device_id, device_name, queue_sync) {
                Ok(_) => {
                    let after = store.stats()?.0;
                    if after > before {
                        report.imported += 1;
                    } else {
                        report.skipped_duplicates += 1;
                    }
                }
                Err(StorageError::Image(_)) | Err(StorageError::InvalidInput(_)) => {
                    report.skipped_empty_or_unsupported += 1;
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            report.skipped_empty_or_unsupported += 1;
        }
    }

    Ok(report)
}

fn build_input_from_maccy(
    row: &MaccyRow,
    entries: &[(String, Vec<u8>)],
    max_image_bytes: usize,
) -> Option<NewClipInput> {
    // Prefer plain text UTIs first.
    for uti in [
        "public.utf8-plain-text",
        "public.url",
        "public.text",
        "com.apple.traditional-mac-plain-text",
    ] {
        if let Some((_, bytes)) = entries.iter().find(|(t, _)| t == uti)
            && let Ok(text) = std::str::from_utf8(bytes)
            && !text.trim().is_empty()
        {
            return Some(NewClipInput {
                kind: ClipKind::Text,
                text: Some(text.to_string()),
                png_bytes: None,
                source_app: row.app.clone(),
                copied_at_ms: Some(row.last_copied_ms),
                pinned: row.pinned,
                copy_count: Some(row.copies as u64),
            });
        }
    }

    // Fallback to image types (PNG directly, or JPEG/TIFF transcoded to PNG).
    for uti in [
        "public.png",
        "com.trolltech.anymime.image--png",
        "public.jpeg",
        "public.tiff",
    ] {
        if let Some((_, bytes)) = entries.iter().find(|(t, _)| t == uti)
            && !bytes.is_empty()
            && bytes.len() <= max_image_bytes * 2
        {
            let png = if (uti == "public.png" || uti == "com.trolltech.anymime.image--png")
                && image::load_from_memory(bytes).is_ok()
            {
                bytes.clone()
            } else if let Ok(img) = image::load_from_memory(bytes) {
                let mut buf = Cursor::new(Vec::new());
                if img.write_to(&mut buf, ImageFormat::Png).is_err() {
                    continue;
                }
                buf.into_inner()
            } else {
                continue;
            };
            if png.len() <= max_image_bytes {
                return Some(NewClipInput {
                    kind: ClipKind::Image,
                    text: None,
                    png_bytes: Some(png),
                    source_app: row.app.clone(),
                    copied_at_ms: Some(row.last_copied_ms),
                    pinned: row.pinned,
                    copy_count: Some(row.copies as u64),
                });
            }
        }
    }

    // Last resort: use ZTITLE if non-empty and not an image placeholder.
    if let Some(title) = row.title.as_deref()
        && !title.trim().is_empty()
        && !entries.iter().any(|(t, _)| t.contains("image") || t.contains("file-url"))
    {
        return Some(NewClipInput {
            kind: ClipKind::Text,
            text: Some(title.to_string()),
            png_bytes: None,
            source_app: row.app.clone(),
            copied_at_ms: Some(row.last_copied_ms),
            pinned: row.pinned,
            copy_count: Some(row.copies as u64),
        });
    }

    None
}

struct MaccyRow {
    pk: i64,
    copies: i64,
    last_copied_ms: i64,
    app: Option<String>,
    pinned: bool,
    title: Option<String>,
}
