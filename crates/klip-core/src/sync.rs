use std::time::Duration;

use reqwest::blocking::Client;
use rusty_s3::{
    Bucket, Credentials, S3Action, UrlStyle,
    actions::{ListObjectsV2, ListObjectsV2Response},
};
use thiserror::Error;
use url::Url;

use crate::config::SyncConfig;
use crate::crypto::{self, CryptoError};
use crate::model::{ClipKind, SyncEvent, SyncOp, SyncReport};
use crate::storage::{ClipStore, StorageError};

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("sync is not configured")]
    NotConfigured,
    #[error("invalid endpoint url: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("s3 config error: {0}")]
    Bucket(#[from] rusty_s3::BucketError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("s3 response status {status}: {body}")]
    RemoteStatus { status: u16, body: String },
    #[error("xml parse error: {0}")]
    Xml(String),
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct S3SyncClient {
    bucket: Bucket,
    credentials: Credentials,
    http: Client,
    prefix: String,
    key: [u8; 32],
}

impl S3SyncClient {
    pub fn from_config(cfg: &SyncConfig, passphrase: Option<&str>) -> Result<Self, SyncError> {
        let key = match (passphrase, cfg.derived_key_b64.as_deref()) {
            (Some(pw), _) => {
                let salt = crypto::decode_salt(&cfg.salt_b64)?;
                crypto::derive_key(pw, &salt)?
            }
            (None, Some(b64)) => crypto::decode_key(b64)?,
            (None, None) => {
                return Err(CryptoError::InvalidKey(
                    "passphrase or cached derived key required".into(),
                )
                .into());
            }
        };

        let endpoint: Url = cfg.endpoint.trim().parse()?;
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            cfg.bucket.clone(),
            cfg.region.clone(),
        )?;
        let credentials = Credentials::new(
            cfg.access_key_id.clone(),
            cfg.secret_access_key.clone(),
        );
        let http = Client::builder().timeout(Duration::from_secs(30)).build()?;
        let prefix = cfg.prefix.trim_matches('/').to_string();

        Ok(Self {
            bucket,
            credentials,
            http,
            prefix,
            key,
        })
    }

    fn scoped_key(&self, rel: &str) -> String {
        let clean = rel.trim_start_matches('/');
        if self.prefix.is_empty() {
            clean.to_string()
        } else {
            format!("{}/{}", self.prefix, clean)
        }
    }

    pub fn sync_now(&self, store: &mut ClipStore, local_device_id: &str) -> Result<SyncReport, SyncError> {
        let mut report = SyncReport {
            synced_at: ClipStore::now_ms(),
            ..Default::default()
        };

        // 1. Push pending blobs & events from local outbox.
        let pending = store.pending_events()?;
        for (event_id, payload_json, blob_hash) in pending {
            if let Some(hash) = blob_hash.as_deref()
                && store.has_blob(hash)
            {
                let blob_key = self.scoped_key(&format!("blobs/{hash}.enc"));
                if !self.head_exists(&blob_key)? {
                    let png = store.read_blob(hash)?;
                    let sealed = crypto::seal(&self.key, blob_key.as_bytes(), &png)?;
                    self.put_object(&blob_key, sealed)?;
                    report.uploaded_blobs += 1;
                }
            }

            let evt_key = self.scoped_key(&format!("events/{local_device_id}/{event_id}.enc"));
            let sealed_evt = crypto::seal(&self.key, evt_key.as_bytes(), payload_json.as_bytes())?;
            self.put_object(&evt_key, sealed_evt)?;
            store.mark_event_sent(&event_id)?;
            report.uploaded_events += 1;
        }

        // 2. Pull unseen remote events from all devices.
        let events_prefix = self.scoped_key("events/");
        let mut keys = self.list_all_keys(&events_prefix)?;
        keys.sort();

        let local_prefix = self.scoped_key(&format!("events/{local_device_id}/"));
        let mut newest_ts: i64 = 0;

        for obj_key in keys {
            if obj_key.starts_with(&local_prefix) || !obj_key.ends_with(".enc") {
                continue;
            }
            if store.is_remote_event_seen(&obj_key)? {
                continue;
            }

            let encrypted = self.get_object(&obj_key)?;
            let plaintext = crypto::open(&self.key, obj_key.as_bytes(), &encrypted)?;
            let evt: SyncEvent = serde_json::from_slice(&plaintext)?;
            report.downloaded_events += 1;

            let blob_bytes = if let SyncOp::Upsert {
                kind: ClipKind::Image,
                blob_hash: Some(hash),
                ..
            } = &evt.op
            {
                if !store.has_blob(hash) {
                    let blob_key = self.scoped_key(&format!("blobs/{hash}.enc"));
                    let enc_blob = self.get_object(&blob_key)?;
                    Some(crypto::open(&self.key, blob_key.as_bytes(), &enc_blob)?)
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(applied) = store.apply_remote_event(&evt, blob_bytes.as_deref())? {
                report.applied_items += 1;
                if applied.last_copied_at >= newest_ts {
                    newest_ts = applied.last_copied_at;
                    report.latest_remote_hash = Some(applied.content_hash);
                }
            }
            store.mark_remote_event_seen(&obj_key)?;
        }

        store.set_meta("last_synced_at", &report.synced_at.to_string())?;
        Ok(report)
    }

    fn head_exists(&self, key: &str) -> Result<bool, SyncError> {
        let action = self.bucket.head_object(Some(&self.credentials), key);
        let url = action.sign(Duration::from_secs(120));
        let resp = self.http.head(url).send()?;
        let status = resp.status().as_u16();
        if status == 200 {
            Ok(true)
        } else if status == 404 {
            Ok(false)
        } else {
            Err(SyncError::RemoteStatus {
                status,
                body: resp.text().unwrap_or_default(),
            })
        }
    }

    fn put_object(&self, key: &str, body: Vec<u8>) -> Result<(), SyncError> {
        let action = self.bucket.put_object(Some(&self.credentials), key);
        let url = action.sign(Duration::from_secs(300));
        let resp = self.http.put(url).body(body).send()?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(SyncError::RemoteStatus {
                status,
                body: resp.text().unwrap_or_default(),
            })
        }
    }

    fn get_object(&self, key: &str) -> Result<Vec<u8>, SyncError> {
        let action = self.bucket.get_object(Some(&self.credentials), key);
        let url = action.sign(Duration::from_secs(300));
        let resp = self.http.get(url).send()?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            Ok(resp.bytes()?.to_vec())
        } else {
            Err(SyncError::RemoteStatus {
                status,
                body: resp.text().unwrap_or_default(),
            })
        }
    }

    fn list_all_keys(&self, prefix: &str) -> Result<Vec<String>, SyncError> {
        let mut all = Vec::new();
        let mut token: Option<String> = None;

        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
            action.with_prefix(prefix);
            if let Some(tok) = token.as_deref() {
                action.with_continuation_token(tok);
            }
            let url = action.sign(Duration::from_secs(120));
            let resp = self.http.get(url).send()?;
            let status = resp.status().as_u16();
            let xml = resp.text()?;
            if !(200..300).contains(&status) {
                return Err(SyncError::RemoteStatus { status, body: xml });
            }
            let parsed: ListObjectsV2Response = ListObjectsV2::parse_response(&xml)
                .map_err(|e| SyncError::Xml(e.to_string()))?;
            for item in parsed.contents {
                all.push(item.key);
            }
            match parsed.next_continuation_token {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        Ok(all)
    }
}
