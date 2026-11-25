use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipKind {
    Text,
    Link,
    Color,
    Image,
}

impl ClipKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Link => "link",
            Self::Color => "color",
            Self::Image => "image",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "link" => Self::Link,
            "color" => Self::Color,
            "image" => Self::Image,
            _ => Self::Text,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipItem {
    pub id: String,
    pub content_hash: String,
    pub kind: ClipKind,
    pub title: String,
    pub preview_text: Option<String>,
    pub color_css: Option<String>,
    pub source_app: Option<String>,
    pub source_device: String,
    pub char_count: Option<u64>,
    pub word_count: Option<u64>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub byte_size: u64,
    pub copy_count: u64,
    pub pinned: bool,
    pub first_copied_at: i64,
    pub last_copied_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    pub thumb_path: Option<String>,
    pub blob_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipDetail {
    pub item: ClipItem,
    pub text_content: Option<String>,
    pub image_data_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipQuery {
    pub search: Option<String>,
    pub kind: Option<ClipKind>,
    pub pinned_only: bool,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewClipInput {
    pub kind: ClipKind,
    pub text: Option<String>,
    pub png_bytes: Option<Vec<u8>>,
    pub source_app: Option<String>,
    pub copied_at_ms: Option<i64>,
    pub pinned: bool,
    pub copy_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncEvent {
    pub event_id: String,
    pub device_id: String,
    pub device_name: String,
    pub created_at: i64,
    pub op: SyncOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncOp {
    Upsert {
        content_hash: String,
        kind: ClipKind,
        text: Option<String>,
        blob_hash: Option<String>,
        image_width: Option<u32>,
        image_height: Option<u32>,
        byte_size: u64,
        source_app: Option<String>,
        first_copied_at: i64,
        last_copied_at: i64,
        copy_count: u64,
        pinned: bool,
    },
    SetPin {
        content_hash: String,
        pinned: bool,
        updated_at: i64,
    },
    Delete {
        content_hash: String,
        deleted_at: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaccyImportReport {
    pub imported: usize,
    pub skipped_duplicates: usize,
    pub skipped_empty_or_unsupported: usize,
    pub db_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub uploaded_events: usize,
    pub uploaded_blobs: usize,
    pub downloaded_events: usize,
    pub applied_items: usize,
    pub latest_remote_hash: Option<String>,
    pub synced_at: i64,
}
