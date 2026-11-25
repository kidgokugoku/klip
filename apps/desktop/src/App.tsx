import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  FileText,
  Image as ImageIcon,
  Link2,
  Pin,
  Search,
} from "lucide-solid";
import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import type {
  AppSettings,
  ClipDetail,
  ClipItem,
  ClipKind,
  FilterValue,
  RetentionMode,
  SyncReport,
  SyncStatusInfo,
} from "./types";

const isMac = navigator.platform.toLowerCase().includes("mac");
const MOD_KEY = isMac ? "⌘" : "Ctrl";
const PAGE_SIZE = 200;

export function App() {
  const [search, setSearch] = createSignal("");
  const [filter, setFilter] = createSignal<FilterValue>("all");
  const [items, setItems] = createSignal<ClipItem[]>([]);
  const [hasMore, setHasMore] = createSignal(false);
  const [loadingMore, setLoadingMore] = createSignal(false);
  const [selectedIdx, setSelectedIdx] = createSignal(0);
  const [detail, setDetail] = createSignal<ClipDetail | null>(null);
  const [isComposing, setIsComposing] = createSignal(false);
  const [actionsOpen, setActionsOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [settingsTab, setSettingsTab] = createSignal<"general" | "sync">(
    "general",
  );
  const [syncStatus, setSyncStatus] = createSignal<SyncStatusInfo | null>(null);
  const [statusFlash, setStatusFlash] = createSignal<string | null>(null);

  // General settings signals
  const [pasteOnEnter, setPasteOnEnter] = createSignal(true);
  const [showTrayIcon, setShowTrayIcon] = createSignal(true);
  const [retentionMode, setRetentionMode] =
    createSignal<RetentionMode>("count");
  const [maxHistoryItems, setMaxHistoryItems] = createSignal(10000);
  const [maxHistoryDays, setMaxHistoryDays] = createSignal(90);
  const [maxImageMb, setMaxImageMb] = createSignal(5);
  const [hotkey, setHotkey] = createSignal(
    isMac ? "Control+Space" : "Alt+Backquote",
  );
  const [ignoredAppsText, setIgnoredAppsText] = createSignal("");

  // Sync form signals
  const [endpoint, setEndpoint] = createSignal("");
  const [bucket, setBucket] = createSignal("clip-sync");
  const [region, setRegion] = createSignal("auto");
  const [prefix, setPrefix] = createSignal("klip");
  const [accessKeyId, setAccessKeyId] = createSignal("");
  const [secretAccessKey, setSecretAccessKey] = createSignal("");
  const [passphrase, setPassphrase] = createSignal("");
  const [autoSyncSecs, setAutoSyncSecs] = createSignal(30);
  const [pushLatest, setPushLatest] = createSignal(true);
  const [busy, setBusy] = createSignal(false);

  let searchInputRef: HTMLInputElement | undefined;
  let rowRefs: HTMLDivElement[] = [];

  const selectedItem = createMemo(() => items()[selectedIdx()] ?? null);

  function flash(msg: string) {
    setStatusFlash(msg);
    setTimeout(() => setStatusFlash(null), 3000);
  }

  function openSettings(tab: "general" | "sync") {
    setActionsOpen(false);
    setSettingsTab(tab);
    setSettingsOpen(true);
    void refreshSettings();
    void refreshSyncStatus();
  }

  function currentQueryParams(offset: number, limit: number) {
    const f = filter();
    const kind: ClipKind | null =
      f === "all" || f === "pinned" ? null : (f as ClipKind);
    return {
      search: search().trim() || null,
      kind,
      pinnedOnly: f === "pinned",
      limit,
      offset,
    };
  }

  async function refreshItems(keepSelection = false) {
    const loadedCount = keepSelection
      ? Math.max(PAGE_SIZE, items().length)
      : PAGE_SIZE;
    const list = await invoke<ClipItem[]>("query_items", {
      query: currentQueryParams(0, loadedCount),
    });
    setItems(list);
    setHasMore(list.length >= loadedCount);
    if (!keepSelection) {
      setSelectedIdx(0);
    } else if (selectedIdx() >= list.length) {
      setSelectedIdx(Math.max(0, list.length - 1));
    }
  }

  async function loadMore() {
    if (!hasMore() || loadingMore()) return;
    setLoadingMore(true);
    try {
      const offset = items().length;
      const next = await invoke<ClipItem[]>("query_items", {
        query: currentQueryParams(offset, PAGE_SIZE),
      });
      if (next.length > 0) {
        setItems((prev) => [...prev, ...next]);
      }
      setHasMore(next.length >= PAGE_SIZE);
    } finally {
      setLoadingMore(false);
    }
  }

  function handleListScroll(e: Event) {
    const el = e.currentTarget as HTMLDivElement;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - 120) {
      void loadMore();
    }
  }

  async function refreshSettings() {
    const s = await invoke<AppSettings>("get_settings");
    setPasteOnEnter(s.pasteOnEnter);
    setShowTrayIcon(s.showTrayIcon);
    setRetentionMode(s.retentionMode);
    setMaxHistoryItems(s.maxHistoryItems);
    setMaxHistoryDays(s.maxHistoryDays);
    setMaxImageMb(s.maxImageMb);
    setHotkey(s.hotkey);
    setIgnoredAppsText(s.ignoredApps.join(", "));
  }

  async function refreshSyncStatus() {
    const info = await invoke<SyncStatusInfo>("get_sync_status");
    setSyncStatus(info);
    if (info.endpoint) setEndpoint(info.endpoint);
    if (info.bucket) setBucket(info.bucket);
    if (info.region) setRegion(info.region);
    if (info.prefix) setPrefix(info.prefix);
    if (info.accessKeyId) setAccessKeyId(info.accessKeyId);
    setAutoSyncSecs(info.autoSyncSecs);
    setPushLatest(info.pushLatestToClipboard);
  }

  createEffect(() => {
    void search();
    void filter();
    void refreshItems(false);
  });

  createEffect(() => {
    const item = selectedItem();
    if (!item) {
      setDetail(null);
      return;
    }
    const targetId = item.id;
    void invoke<ClipDetail | null>("get_item_detail", { id: targetId }).then(
      (res) => {
        if (selectedItem()?.id === targetId) {
          setDetail(res);
        }
      },
    );
  });

  createEffect(() => {
    const idx = selectedIdx();
    const el = rowRefs[idx];
    el?.scrollIntoView({ block: "nearest" });
    if (hasMore() && idx >= items().length - 15) {
      void loadMore();
    }
  });

  async function handlePaste(pasteAfter: boolean) {
    const item = selectedItem();
    if (!item) return;
    setActionsOpen(false);
    try {
      await invoke("copy_item", { id: item.id, pasteAfter });
    } catch (e) {
      flash(String(e));
    }
  }

  async function handleTogglePin() {
    const item = selectedItem();
    if (!item) return;
    setActionsOpen(false);
    await invoke("toggle_pin", { id: item.id });
    await refreshItems(true);
  }

  async function handleDelete() {
    const item = selectedItem();
    if (!item) return;
    setActionsOpen(false);
    await invoke("delete_item", { id: item.id });
    await refreshItems(true);
    await refreshSyncStatus();
  }

  async function handleClearUnpinned() {
    setActionsOpen(false);
    const removed = await invoke<number>("clear_unpinned");
    await refreshItems(false);
    await refreshSyncStatus();
    flash(`Cleared ${removed} items`);
  }

  async function handleSyncNow() {
    setActionsOpen(false);
    setBusy(true);
    try {
      const rep = await invoke<SyncReport>("sync_now");
      await refreshItems(true);
      await refreshSyncStatus();
      flash(`Synced ↑${rep.uploadedEvents} ↓${rep.downloadedEvents}`);
    } catch (e) {
      flash(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleSaveGeneral(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      const ignoredApps = ignoredAppsText()
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      await invoke<AppSettings>("update_settings", {
        settings: {
          pasteOnEnter: pasteOnEnter(),
          showTrayIcon: showTrayIcon(),
          retentionMode: retentionMode(),
          maxHistoryItems: Math.max(1, Number(maxHistoryItems()) || 10000),
          maxHistoryDays: Math.max(1, Number(maxHistoryDays()) || 90),
          maxImageMb: Math.max(1, Number(maxImageMb()) || 5),
          hotkey: hotkey().trim(),
          ignoredApps,
        },
      });
      setSettingsOpen(false);
      await refreshItems(true);
      await refreshSyncStatus();
      flash("Settings saved");
    } catch (err) {
      flash(String(err));
    } finally {
      setBusy(false);
    }
  }

  async function handleSaveSync(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      const rep = await invoke<SyncReport>("setup_sync", {
        req: {
          endpoint: endpoint(),
          bucket: bucket(),
          region: region(),
          prefix: prefix(),
          accessKeyId: accessKeyId(),
          secretAccessKey: secretAccessKey(),
          passphrase: passphrase(),
          autoSyncSecs: Number(autoSyncSecs()),
          pushLatestToClipboard: pushLatest(),
        },
      });
      setSettingsOpen(false);
      setSecretAccessKey("");
      setPassphrase("");
      await refreshItems(true);
      await refreshSyncStatus();
      flash(`R2 connected (↑${rep.uploadedEvents} ↓${rep.downloadedEvents})`);
    } catch (err) {
      flash(String(err));
    } finally {
      setBusy(false);
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (isComposing() || e.isComposing || e.keyCode === 229) {
      return;
    }

    const mod = isMac ? e.metaKey : e.ctrlKey;

    if (mod && e.key === ",") {
      e.preventDefault();
      openSettings("general");
      return;
    }

    if (settingsOpen()) {
      if (e.key === "Escape") {
        e.preventDefault();
        setSettingsOpen(false);
      }
      return;
    }

    if (e.key === "Escape") {
      e.preventDefault();
      if (actionsOpen()) {
        setActionsOpen(false);
      } else {
        void invoke("hide_panel");
      }
      return;
    }

    if (mod && e.key.toLowerCase() === "k") {
      e.preventDefault();
      setActionsOpen((v) => !v);
      return;
    }

    if (actionsOpen()) {
      return;
    }

    if (e.key === "ArrowDown" || (e.ctrlKey && e.key.toLowerCase() === "n")) {
      e.preventDefault();
      setSelectedIdx((i) => Math.min(i + 1, Math.max(0, items().length - 1)));
      return;
    }

    if (e.key === "ArrowUp" || (e.ctrlKey && e.key.toLowerCase() === "p")) {
      e.preventDefault();
      setSelectedIdx((i) => Math.max(0, i - 1));
      return;
    }

    if (e.key === "Enter") {
      e.preventDefault();
      const paste = mod ? !pasteOnEnter() : pasteOnEnter();
      void handlePaste(paste);
      return;
    }

    if (mod && e.shiftKey && e.key.toLowerCase() === "p") {
      e.preventDefault();
      void handleTogglePin();
      return;
    }

    if (mod && (e.key === "Backspace" || e.key === "Delete")) {
      e.preventDefault();
      void handleDelete();
      return;
    }
  }

  onMount(() => {
    void refreshItems(false);
    void refreshSettings();
    void refreshSyncStatus();

    const unlistenShown = listen("klip://shown", () => {
      setActionsOpen(false);
      setSettingsOpen(false);
      setSelectedIdx(0);
      void refreshItems(false);
      void refreshSettings();
      void refreshSyncStatus();
      requestAnimationFrame(() => {
        searchInputRef?.focus();
        searchInputRef?.select();
      });
    });

    const unlistenUpdated = listen("klip://updated", () => {
      void refreshItems(true);
      void refreshSyncStatus();
    });

    window.addEventListener("keydown", handleKeyDown);
    onCleanup(() => {
      window.removeEventListener("keydown", handleKeyDown);
      void unlistenShown.then((fn) => fn());
      void unlistenUpdated.then((fn) => fn());
    });
  });

  const syncDotClass = createMemo(() => {
    const s = syncStatus();
    if (!s || !s.enabled) return "dot";
    if (s.lastError) return "dot err";
    if (s.pendingEvents > 0) return "dot pending";
    return "dot ok";
  });

  const countEnabled = createMemo(
    () => retentionMode() === "count" || retentionMode() === "both",
  );
  const daysEnabled = createMemo(
    () => retentionMode() === "days" || retentionMode() === "both",
  );

  const footerCountLabel = createMemo(() => {
    const total = syncStatus()?.totalItems ?? items().length;
    const isFiltered = search().trim().length > 0 || filter() !== "all";
    if (!isFiltered) {
      return `${total} items`;
    }
    return hasMore()
      ? `${items().length}+ matches (${total} total)`
      : `${items().length} matches (${total} total)`;
  });

  return (
    <div class="shell">
      <div class="header">
        <span class="search-icon">
          <Search size={16} />
        </span>
        <input
          ref={searchInputRef}
          class="search-input"
          type="text"
          placeholder="Search clipboard history..."
          value={search()}
          onInput={(e) => setSearch(e.currentTarget.value)}
          onCompositionStart={() => setIsComposing(true)}
          onCompositionEnd={() => setIsComposing(false)}
          autofocus
        />
        <select
          class="filter-select"
          value={filter()}
          onChange={(e) => setFilter(e.currentTarget.value as FilterValue)}
        >
          <option value="all">All Types</option>
          <option value="text">Text</option>
          <option value="link">Links</option>
          <option value="color">Colors</option>
          <option value="image">Images</option>
          <option value="pinned">Pinned</option>
        </select>
        <button
          class="sync-pill"
          type="button"
          onClick={() => openSettings(syncStatus()?.enabled ? "general" : "sync")}
          title="Settings"
        >
          <span class={syncDotClass()} />
          <span>{syncStatus()?.enabled ? "R2" : "Local"}</span>
        </button>
      </div>

      <div class="main">
        <div class="list-pane" onScroll={handleListScroll}>
          <Show
            when={items().length > 0}
            fallback={<div class="empty">No clipboard items</div>}
          >
            <For each={items()}>
              {(item, idx) => (
                <div
                  ref={(el) => {
                    rowRefs[idx()] = el;
                  }}
                  class="row"
                  classList={{ selected: selectedIdx() === idx() }}
                  onClick={() => setSelectedIdx(idx())}
                  onDblClick={() => void handlePaste(pasteOnEnter())}
                >
                  <div class="row-icon">
                    {item.kind === "color" && item.colorCss ? (
                      <span
                        class="row-swatch"
                        style={{ background: item.colorCss }}
                      />
                    ) : item.kind === "image" ? (
                      <ImageIcon size={13} />
                    ) : item.kind === "link" ? (
                      <Link2 size={13} />
                    ) : (
                      <FileText size={13} />
                    )}
                  </div>
                  <div class="row-title">{item.title}</div>
                  <div class="row-meta">
                    <Show when={item.pinned}>
                      <span class="pin-mark">
                        <Pin size={11} />
                      </span>
                    </Show>
                    <span>{formatRelativeTime(item.lastCopiedAt)}</span>
                  </div>
                </div>
              )}
            </For>
          </Show>
        </div>

        <div class="preview-pane">
          <Show
            when={selectedItem()}
            fallback={<div class="empty">Select an item</div>}
          >
            {(item) => (
              <>
                <div class="preview-body">
                  <Show when={item().kind === "image"}>
                    <div class="preview-image-wrap">
                      <Show
                        when={detail()?.imageDataUrl}
                        fallback={<div class="empty">Loading image...</div>}
                      >
                        {(src) => (
                          <img class="preview-image" src={src()} alt="clipboard" />
                        )}
                      </Show>
                    </div>
                  </Show>

                  <Show when={item().kind === "color" && item().colorCss}>
                    <div class="preview-color-wrap">
                      <div
                        class="preview-color-swatch"
                        style={{ background: item().colorCss! }}
                      />
                      <div class="preview-color-code">
                        {detail()?.textContent ?? item().colorCss}
                      </div>
                    </div>
                  </Show>

                  <Show when={item().kind === "link"}>
                    <div class="preview-link-card">
                      <div class="preview-link-host">
                        {extractHost(detail()?.textContent ?? item().title)}
                      </div>
                      <div class="preview-link-url">
                        {detail()?.textContent ?? item().title}
                      </div>
                    </div>
                  </Show>

                  <Show when={item().kind === "text"}>
                    <pre class="preview-text">
                      {detail()?.textContent ?? item().previewText ?? item().title}
                    </pre>
                  </Show>
                </div>

                <div class="meta-panel">
                  <div class="meta-row">
                    <span class="meta-key">Application</span>
                    <span class="meta-val">{item().sourceApp ?? "Unknown"}</span>
                  </div>
                  <div class="meta-row">
                    <span class="meta-key">Content type</span>
                    <span class="meta-val">{kindLabel(item().kind)}</span>
                  </div>
                  <Show
                    when={item().kind === "image"}
                    fallback={
                      <div class="meta-row">
                        <span class="meta-key">Characters / Words</span>
                        <span class="meta-val">
                          {item().charCount ?? 0} / {item().wordCount ?? 0}
                        </span>
                      </div>
                    }
                  >
                    <div class="meta-row">
                      <span class="meta-key">Dimensions</span>
                      <span class="meta-val">
                        {item().imageWidth ?? 0} × {item().imageHeight ?? 0}
                      </span>
                    </div>
                  </Show>
                  <div class="meta-row">
                    <span class="meta-key">Times copied</span>
                    <span class="meta-val">{item().copyCount}</span>
                  </div>
                  <div class="meta-row">
                    <span class="meta-key">Last copied</span>
                    <span class="meta-val">
                      {formatFullTime(item().lastCopiedAt)} · {item().sourceDevice}
                    </span>
                  </div>
                </div>
              </>
            )}
          </Show>
        </div>
      </div>

      <div class="footer">
        <div class="footer-left">
          <span>{statusFlash() ?? footerCountLabel()}</span>
        </div>
        <div class="footer-actions">
          <button
            class="action-btn"
            type="button"
            onClick={() => void handlePaste(pasteOnEnter())}
            disabled={!selectedItem()}
          >
            <span>{pasteOnEnter() ? "Paste" : "Copy"}</span>
            <kbd>↵</kbd>
          </button>
          <button
            class="action-btn"
            type="button"
            onClick={() => setActionsOpen((v) => !v)}
          >
            <span>Actions</span>
            <kbd>{MOD_KEY}K</kbd>
          </button>
        </div>
      </div>

      <Show when={actionsOpen()}>
        <div class="popover-backdrop" onClick={() => setActionsOpen(false)}>
          <div class="action-menu" onClick={(e) => e.stopPropagation()}>
            <button
              class="menu-item"
              type="button"
              onClick={() => void handlePaste(true)}
            >
              <span>Paste to Active App</span>
              <kbd>{pasteOnEnter() ? "↵" : `${MOD_KEY}↵`}</kbd>
            </button>
            <button
              class="menu-item"
              type="button"
              onClick={() => void handlePaste(false)}
            >
              <span>Copy to Clipboard</span>
              <kbd>{pasteOnEnter() ? `${MOD_KEY}↵` : "↵"}</kbd>
            </button>
            <button
              class="menu-item"
              type="button"
              onClick={() => void handleTogglePin()}
            >
              <span>{selectedItem()?.pinned ? "Unpin" : "Pin"}</span>
              <kbd>{MOD_KEY}⇧P</kbd>
            </button>
            <button
              class="menu-item danger"
              type="button"
              onClick={() => void handleDelete()}
            >
              <span>Delete Item</span>
              <kbd>{MOD_KEY}⌫</kbd>
            </button>
            <div class="menu-sep" />
            <button
              class="menu-item"
              type="button"
              onClick={() => void handleSyncNow()}
            >
              <span>Sync R2 Now</span>
            </button>
            <button
              class="menu-item"
              type="button"
              onClick={() => openSettings("general")}
            >
              <span>Settings...</span>
              <kbd>{MOD_KEY},</kbd>
            </button>
            <div class="menu-sep" />
            <button
              class="menu-item danger"
              type="button"
              onClick={() => void handleClearUnpinned()}
            >
              <span>Clear Unpinned</span>
            </button>
          </div>
        </div>
      </Show>

      <Show when={settingsOpen()}>
        <div class="modal-backdrop" onClick={() => setSettingsOpen(false)}>
          <div class="modal" onClick={(e) => e.stopPropagation()}>
            <div class="modal-header">
              <div class="modal-title">Settings</div>
              <div class="modal-tabs">
                <button
                  type="button"
                  class="tab-btn"
                  classList={{ active: settingsTab() === "general" }}
                  onClick={() => setSettingsTab("general")}
                >
                  General
                </button>
                <button
                  type="button"
                  class="tab-btn"
                  classList={{ active: settingsTab() === "sync" }}
                  onClick={() => setSettingsTab("sync")}
                >
                  R2 Sync
                </button>
              </div>
            </div>

            <Show when={settingsTab() === "general"}>
              <form
                style={{ display: "flex", "flex-direction": "column", gap: "10px" }}
                onSubmit={handleSaveGeneral}
              >
                <label class="check-row">
                  <input
                    type="checkbox"
                    checked={pasteOnEnter()}
                    onChange={(e) => setPasteOnEnter(e.currentTarget.checked)}
                  />
                  <span>Paste immediately on Enter</span>
                </label>

                <label class="check-row">
                  <input
                    type="checkbox"
                    checked={showTrayIcon()}
                    onChange={(e) => setShowTrayIcon(e.currentTarget.checked)}
                  />
                  <span>Show menu bar / tray icon</span>
                </label>

                <div class="field-grid">
                  <div class="field">
                    <label>Global Hotkey</label>
                    <input
                      value={hotkey()}
                      onInput={(e) => setHotkey(e.currentTarget.value)}
                      required
                    />
                  </div>
                  <div class="field">
                    <label>Max Image Size (MB)</label>
                    <input
                      type="number"
                      min={1}
                      max={100}
                      value={maxImageMb()}
                      onInput={(e) => setMaxImageMb(Number(e.currentTarget.value))}
                    />
                  </div>
                </div>

                <div class="field">
                  <label>History Retention Mode</label>
                  <select
                    value={retentionMode()}
                    onChange={(e) =>
                      setRetentionMode(e.currentTarget.value as RetentionMode)
                    }
                  >
                    <option value="both">Both (Max Items & Max Days)</option>
                    <option value="count">Max Items</option>
                    <option value="days">Max Days</option>
                    <option value="unlimited">Unlimited</option>
                  </select>
                </div>

                <div class="field-grid">
                  <div class="field">
                    <label>Max Items</label>
                    <input
                      type="number"
                      min={1}
                      disabled={!countEnabled()}
                      value={maxHistoryItems()}
                      onInput={(e) =>
                        setMaxHistoryItems(Number(e.currentTarget.value))
                      }
                    />
                  </div>
                  <div class="field">
                    <label>Max Age (Days)</label>
                    <input
                      type="number"
                      min={1}
                      disabled={!daysEnabled()}
                      value={maxHistoryDays()}
                      onInput={(e) =>
                        setMaxHistoryDays(Number(e.currentTarget.value))
                      }
                    />
                  </div>
                </div>

                <div class="field">
                  <label>Ignored Apps (comma-separated)</label>
                  <input
                    value={ignoredAppsText()}
                    onInput={(e) => setIgnoredAppsText(e.currentTarget.value)}
                  />
                </div>

                <div class="modal-footer">
                  <span />
                  <div style={{ display: "flex", gap: "8px" }}>
                    <button
                      class="btn"
                      type="button"
                      onClick={() => setSettingsOpen(false)}
                    >
                      Cancel
                    </button>
                    <button class="btn primary" type="submit" disabled={busy()}>
                      {busy() ? "Saving..." : "Save"}
                    </button>
                  </div>
                </div>
              </form>
            </Show>

            <Show when={settingsTab() === "sync"}>
              <form
                style={{ display: "flex", "flex-direction": "column", gap: "10px" }}
                onSubmit={handleSaveSync}
              >
                <div class="field">
                  <label>Endpoint</label>
                  <input
                    placeholder="https://<account-id>.r2.cloudflarestorage.com"
                    value={endpoint()}
                    onInput={(e) => setEndpoint(e.currentTarget.value)}
                    required
                  />
                </div>
                <div class="field-grid">
                  <div class="field">
                    <label>Bucket</label>
                    <input
                      value={bucket()}
                      onInput={(e) => setBucket(e.currentTarget.value)}
                      required
                    />
                  </div>
                  <div class="field">
                    <label>Prefix</label>
                    <input
                      value={prefix()}
                      onInput={(e) => setPrefix(e.currentTarget.value)}
                    />
                  </div>
                </div>
                <div class="field-grid">
                  <div class="field">
                    <label>Region</label>
                    <input
                      value={region()}
                      onInput={(e) => setRegion(e.currentTarget.value)}
                    />
                  </div>
                  <div class="field">
                    <label>Interval (sec)</label>
                    <input
                      type="number"
                      min={5}
                      value={autoSyncSecs()}
                      onInput={(e) =>
                        setAutoSyncSecs(Number(e.currentTarget.value))
                      }
                    />
                  </div>
                </div>
                <div class="field">
                  <label>Access Key ID</label>
                  <input
                    value={accessKeyId()}
                    onInput={(e) => setAccessKeyId(e.currentTarget.value)}
                    required
                  />
                </div>
                <div class="field">
                  <label>
                    <span>Secret Access Key</span>
                    <Show when={syncStatus()?.hasSecretAccessKey}>
                      <span class="saved-badge">Saved</span>
                    </Show>
                  </label>
                  <input
                    type="password"
                    placeholder={
                      syncStatus()?.hasSecretAccessKey
                        ? "Saved — enter new value to overwrite"
                        : "Enter R2 Secret Access Key"
                    }
                    value={secretAccessKey()}
                    onInput={(e) => setSecretAccessKey(e.currentTarget.value)}
                    required={!syncStatus()?.hasSecretAccessKey}
                  />
                </div>
                <div class="field">
                  <label>
                    <span>E2EE Passphrase</span>
                    <Show when={syncStatus()?.hasPassphrase}>
                      <span class="saved-badge">Saved</span>
                    </Show>
                  </label>
                  <input
                    type="password"
                    placeholder={
                      syncStatus()?.hasPassphrase
                        ? "Saved — enter new passphrase to overwrite"
                        : "Enter E2EE Passphrase"
                    }
                    value={passphrase()}
                    onInput={(e) => setPassphrase(e.currentTarget.value)}
                    required={!syncStatus()?.hasPassphrase}
                  />
                </div>
                <label class="check-row">
                  <input
                    type="checkbox"
                    checked={pushLatest()}
                    onChange={(e) => setPushLatest(e.currentTarget.checked)}
                  />
                  <span>Push latest remote item to local clipboard</span>
                </label>
                <div class="modal-footer">
                  <span />
                  <div style={{ display: "flex", gap: "8px" }}>
                    <button
                      class="btn"
                      type="button"
                      onClick={() => setSettingsOpen(false)}
                    >
                      Cancel
                    </button>
                    <button class="btn primary" type="submit" disabled={busy()}>
                      {busy() ? "Syncing..." : "Save & Sync"}
                    </button>
                  </div>
                </div>
              </form>
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}

function kindLabel(kind: ClipKind): string {
  switch (kind) {
    case "text":
      return "Plain Text";
    case "link":
      return "Link";
    case "color":
      return "Color";
    case "image":
      return "Image (PNG)";
  }
}

function extractHost(url: string): string {
  try {
    return new URL(url.trim()).host;
  } catch {
    return "Link";
  }
}

function formatRelativeTime(ms: number): string {
  const diffSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (diffSec < 60) return "now";
  const mins = Math.floor(diffSec / 60);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h`;
  const days = Math.floor(hrs / 24);
  return `${days}d`;
}

function formatFullTime(ms: number): string {
  return new Date(ms).toLocaleString();
}
