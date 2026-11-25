use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use clipboard_rs::{Clipboard, ClipboardContext, RustImageData, common::RustImage};
use klip_core::config::{
    RetentionMode, SyncConfig, default_data_dir, load_or_create_config, save_config,
};
use klip_core::crypto;
use klip_core::maccy::import_from_maccy;
use klip_core::model::{ClipKind, ClipQuery};
use klip_core::storage::ClipStore;
use klip_core::sync::S3SyncClient;

#[derive(Parser)]
#[command(name = "klip", version, about = "Raycast-style cross-platform clipboard CLI & E2EE sync")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List recent clipboard items
    List {
        #[arg(long, value_enum)]
        kind: Option<KindArg>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long)]
        pinned: bool,
        #[arg(long)]
        json: bool,
    },
    /// Search clipboard history using FTS5
    Search {
        query: String,
        #[arg(long, value_enum)]
        kind: Option<KindArg>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Copy an item back to the system clipboard by ID
    Copy { id: String },
    /// Read-only import from Maccy Storage.sqlite
    ImportMaccy {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, help = "Also enqueue imported items for R2 sync")]
        sync: bool,
    },
    /// Manage E2EE Cloudflare R2 / S3 sync
    Sync {
        #[command(subcommand)]
        sub: SyncCmd,
    },
    /// Configure general settings
    Config {
        #[arg(long)]
        paste_on_enter: Option<bool>,
        #[arg(long)]
        tray_icon: Option<bool>,
        #[arg(long, value_enum)]
        retention: Option<RetentionArg>,
        #[arg(long)]
        max_items: Option<usize>,
        #[arg(long)]
        max_days: Option<u32>,
        #[arg(long)]
        max_image_mb: Option<usize>,
    },
    /// Prune expired/excess unpinned items using current retention settings
    Prune,
    /// Show data directory, stats, and config summary
    Info,
}

#[derive(Clone, Copy, ValueEnum)]
enum RetentionArg {
    Count,
    Days,
    Both,
    Unlimited,
}

impl From<RetentionArg> for RetentionMode {
    fn from(r: RetentionArg) -> Self {
        match r {
            RetentionArg::Count => RetentionMode::Count,
            RetentionArg::Days => RetentionMode::Days,
            RetentionArg::Both => RetentionMode::Both,
            RetentionArg::Unlimited => RetentionMode::Unlimited,
        }
    }
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Configure R2 / S3 credentials and E2EE passphrase interactively (or 2 stdin lines: secret + passphrase)
    Setup {
        #[arg(long)]
        bucket: String,
        #[arg(long, help = "R2 / S3 endpoint URL, e.g. https://<account-id>.r2.cloudflarestorage.com")]
        endpoint: String,
        #[arg(long, default_value = "auto")]
        region: String,
        #[arg(long, default_value = "klip")]
        prefix: String,
        #[arg(long)]
        access_key_id: String,
        #[arg(long, default_value_t = 30)]
        auto_sync_secs: u64,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        push_latest_to_clipboard: bool,
    },
    /// Run E2EE sync now
    Now,
    /// Show sync status
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Text,
    Link,
    Color,
    Image,
}

impl From<KindArg> for ClipKind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Text => ClipKind::Text,
            KindArg::Link => ClipKind::Link,
            KindArg::Color => ClipKind::Color,
            KindArg::Image => ClipKind::Image,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = default_data_dir()?;
    let mut cfg = load_or_create_config(&data_dir)?;
    let mut store = ClipStore::open(&data_dir)?;

    match cli.cmd {
        Command::List {
            kind,
            limit,
            pinned,
            json,
        } => {
            let items = store.query_items(&ClipQuery {
                search: None,
                kind: kind.map(Into::into),
                pinned_only: pinned,
                limit: Some(limit),
                offset: Some(0),
            })?;
            print_items(&items, json)?;
        }
        Command::Search {
            query,
            kind,
            limit,
            json,
        } => {
            let items = store.query_items(&ClipQuery {
                search: Some(query),
                kind: kind.map(Into::into),
                pinned_only: false,
                limit: Some(limit),
                offset: Some(0),
            })?;
            print_items(&items, json)?;
        }
        Command::Copy { id } => {
            let Some((item, text_content)) = store.get_by_id(&id)? else {
                bail!("item not found: {id}");
            };
            let ctx = ClipboardContext::new().map_err(|e| anyhow::anyhow!("{e}"))?;
            match item.kind {
                ClipKind::Image => {
                    let bytes = store
                        .read_image_bytes(&item)?
                        .context("image blob missing on disk")?;
                    let img = RustImageData::from_bytes(&bytes)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    ctx.set_image(img).map_err(|e| anyhow::anyhow!("{e}"))?;
                }
                _ => {
                    let text = text_content.unwrap_or_else(|| item.title.clone());
                    ctx.set_text(text).map_err(|e| anyhow::anyhow!("{e}"))?;
                }
            }
            println!("copied {} ({})", item.id, item.kind.as_str());
        }
        Command::ImportMaccy { path, sync } => {
            let report = import_from_maccy(
                &mut store,
                path.as_deref(),
                &cfg.device_id,
                &cfg.device_name,
                sync,
                cfg.max_image_bytes,
            )?;
            println!(
                "imported={} duplicates={} skipped={} from {}",
                report.imported,
                report.skipped_duplicates,
                report.skipped_empty_or_unsupported,
                report.db_path
            );
        }
        Command::Sync { sub } => match sub {
            SyncCmd::Setup {
                bucket,
                endpoint,
                region,
                prefix,
                access_key_id,
                auto_sync_secs,
                push_latest_to_clipboard,
            } => {
                let (secret, passphrase) = read_two_secrets()?;
                let salt = crypto::deterministic_vault_salt(&format!("{bucket}/{prefix}"));
                let derived = crypto::derive_key(&passphrase, &salt)?;
                let sync_cfg = SyncConfig {
                    endpoint,
                    bucket,
                    region,
                    prefix,
                    access_key_id,
                    secret_access_key: secret,
                    salt_b64: crypto::encode_bytes(&salt),
                    derived_key_b64: Some(crypto::encode_bytes(&derived)),
                    auto_sync_secs,
                    push_latest_to_clipboard,
                };
                let client = S3SyncClient::from_config(&sync_cfg, None)?;
                let report = client.sync_now(&mut store, &cfg.device_id)?;
                cfg.sync = Some(sync_cfg);
                save_config(&data_dir, &cfg)?;
                println!(
                    "sync configured and verified: up_events={} up_blobs={} down_events={} applied={}",
                    report.uploaded_events,
                    report.uploaded_blobs,
                    report.downloaded_events,
                    report.applied_items
                );
            }
            SyncCmd::Now => {
                let Some(sync_cfg) = cfg.sync.as_ref() else {
                    bail!("sync not configured; run `klip sync setup` first");
                };
                let client = S3SyncClient::from_config(sync_cfg, None)?;
                let report = client.sync_now(&mut store, &cfg.device_id)?;
                println!(
                    "synced: up_events={} up_blobs={} down_events={} applied={}",
                    report.uploaded_events,
                    report.uploaded_blobs,
                    report.downloaded_events,
                    report.applied_items
                );
            }
            SyncCmd::Status => {
                let (total, pending) = store.stats()?;
                let last_sync = store.get_meta("last_synced_at")?;
                match &cfg.sync {
                    Some(s) => println!(
                        "enabled: s3://{}/{} ({}) | interval={}s | total={} pending={} | last_sync={}",
                        s.bucket,
                        s.prefix,
                        s.endpoint,
                        s.auto_sync_secs,
                        total,
                        pending,
                        last_sync.as_deref().unwrap_or("never")
                    ),
                    None => println!("disabled | total={} pending={}", total, pending),
                }
            }
        },
        Command::Config {
            paste_on_enter,
            tray_icon,
            retention,
            max_items,
            max_days,
            max_image_mb,
        } => {
            if let Some(v) = paste_on_enter {
                cfg.paste_on_enter = v;
            }
            if let Some(v) = tray_icon {
                cfg.show_tray_icon = v;
            }
            if let Some(r) = retention {
                cfg.retention_mode = r.into();
            }
            if let Some(n) = max_items {
                cfg.max_history_items = n.max(1);
            }
            if let Some(d) = max_days {
                cfg.max_history_days = d.max(1);
            }
            if let Some(mb) = max_image_mb {
                cfg.max_image_bytes = mb.clamp(1, 100) * 1024 * 1024;
            }
            save_config(&data_dir, &cfg)?;
            let pruned = store.prune(cfg.retention_mode, cfg.max_history_items, cfg.max_history_days)?;
            println!(
                "saved config: paste_on_enter={} tray_icon={} retention={:?} max_items={} max_days={} max_image_mb={} (pruned={})",
                cfg.paste_on_enter,
                cfg.show_tray_icon,
                cfg.retention_mode,
                cfg.max_history_items,
                cfg.max_history_days,
                cfg.max_image_bytes / (1024 * 1024),
                pruned
            );
        }
        Command::Prune => {
            let pruned = store.prune(cfg.retention_mode, cfg.max_history_items, cfg.max_history_days)?;
            println!("pruned {pruned} unpinned items");
        }
        Command::Info => {
            let (total, pending) = store.stats()?;
            println!("data_dir: {}", data_dir.display());
            println!("device: {} ({})", cfg.device_name, cfg.device_id);
            println!("hotkey_macos: {}", cfg.hotkey_macos);
            println!("hotkey_windows: {}", cfg.hotkey_windows);
            println!("paste_on_enter: {}", cfg.paste_on_enter);
            println!("show_tray_icon: {}", cfg.show_tray_icon);
            println!(
                "retention: {:?} (max_items={}, max_days={}, max_image_mb={})",
                cfg.retention_mode,
                cfg.max_history_items,
                cfg.max_history_days,
                cfg.max_image_bytes / (1024 * 1024)
            );
            println!("items: {total} (pending_sync: {pending})");
            println!(
                "sync: {}",
                cfg.sync
                    .as_ref()
                    .map(|s| format!("s3://{}/{}", s.bucket, s.prefix))
                    .unwrap_or_else(|| "disabled".into())
            );
        }
    }

    Ok(())
}

fn print_items(items: &[klip_core::model::ClipItem], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(items)?);
        return Ok(());
    }
    for item in items {
        let pin = if item.pinned { "*" } else { " " };
        let app = item.source_app.as_deref().unwrap_or("-");
        println!(
            "{} [{}] {:<5} ({}) {}",
            pin,
            item.id,
            item.kind.as_str(),
            app,
            item.title
        );
    }
    Ok(())
}

fn read_two_secrets() -> Result<(String, String)> {
    if io::stdin().is_terminal() {
        let secret = rpassword::prompt_password("R2 Secret Access Key: ")?;
        let pass = rpassword::prompt_password("E2EE Passphrase: ")?;
        if secret.trim().is_empty() || pass.is_empty() {
            bail!("secret and passphrase must not be empty");
        }
        Ok((secret.trim().to_string(), pass))
    } else {
        let mut lines = io::stdin().lock().lines();
        let secret = lines
            .next()
            .transpose()?
            .context("expected secret on stdin line 1")?;
        let pass = lines
            .next()
            .transpose()?
            .context("expected passphrase on stdin line 2")?;
        if secret.trim().is_empty() || pass.is_empty() {
            bail!("secret and passphrase must not be empty");
        }
        Ok((secret.trim().to_string(), pass))
    }
}
