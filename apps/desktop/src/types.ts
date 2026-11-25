export type ClipKind = "text" | "link" | "color" | "image";

export interface ClipItem {
  id: string;
  contentHash: string;
  kind: ClipKind;
  title: string;
  previewText?: string | null;
  colorCss?: string | null;
  sourceApp?: string | null;
  sourceDevice: string;
  charCount?: number | null;
  wordCount?: number | null;
  imageWidth?: number | null;
  imageHeight?: number | null;
  byteSize: number;
  copyCount: number;
  pinned: boolean;
  firstCopiedAt: number;
  lastCopiedAt: number;
  updatedAt: number;
  deletedAt?: number | null;
  thumbPath?: string | null;
  blobPath?: string | null;
}

export interface ClipDetail {
  item: ClipItem;
  textContent?: string | null;
  imageDataUrl?: string | null;
}

export interface SyncStatusInfo {
  enabled: boolean;
  bucket?: string | null;
  endpoint?: string | null;
  region?: string | null;
  prefix?: string | null;
  accessKeyId?: string | null;
  hasSecretAccessKey: boolean;
  hasPassphrase: boolean;
  autoSyncSecs: number;
  pushLatestToClipboard: boolean;
  totalItems: number;
  pendingEvents: number;
  lastSyncedAt?: number | null;
  lastError?: string | null;
  hotkey: string;
  deviceName: string;
}

export interface SyncReport {
  uploadedEvents: number;
  uploadedBlobs: number;
  downloadedEvents: number;
  appliedItems: number;
  latestRemoteHash?: string | null;
  syncedAt: number;
}

export type RetentionMode = "count" | "days" | "both" | "unlimited";

export interface AppSettings {
  pasteOnEnter: boolean;
  showTrayIcon: boolean;
  retentionMode: RetentionMode;
  maxHistoryItems: number;
  maxHistoryDays: number;
  maxImageMb: number;
  hotkey: string;
  ignoredApps: string[];
}

export type FilterValue = "all" | "pinned" | ClipKind;
