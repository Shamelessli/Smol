import { useEffect, useState } from "react";
import { ArrowUp, File, Folder, Loader2, Smartphone, X } from "lucide-react";
import { listDeviceDir } from "@/lib/tauri";
import type { DeviceEntry } from "@/lib/tauri";
import { formatBytesExact } from "@/lib/format";
import { ScrollArea } from "@/components/ui/scroll-area";

// ── Device paths ──────────────────────────────────────────────────────────────
// Android's /sdcard is a symlink to /storage/emulated/0 on essentially every
// device — a safe starting point for the browser.
const ROOT_DIR = "/sdcard";
const COMMON_DIRS = ["DCIM", "Pictures", "Download", "Movies", "Music"];

const USB_DEBUG_GUIDANCE = "未检测到设备或未授权 — 请启用 USB 调试并授权";

function joinRemote(dir: string, name: string): string {
  return `${dir.replace(/\/+$/, "")}/${name}`;
}

function parentRemote(dir: string): string {
  const trimmed = dir.replace(/\/+$/, "");
  if (!trimmed.includes("/")) return "/";
  return trimmed.slice(0, trimmed.lastIndexOf("/")) || "/";
}

interface DeviceBrowserProps {
  open: boolean;
  onClose: () => void;
  /** File mode: emits selected remote file paths. Directory mode: emits the current remote dir. */
  onSelect: (paths: string[]) => void;
  /** When true, the confirm button returns the current directory instead of selected files. */
  pickDirectory?: boolean;
}

/**
 * Modal browser for the connected Android device (via adb).
 *
 * File mode (default): dirs navigate, files are multi-selected with checkboxes,
 * "Add N files" emits the selected remote paths. Directory mode: the confirm
 * button returns the currently navigated directory — used for `android-folder`
 * delivery. Any `adb` failure shows USB-debugging guidance instead of the list.
 */
export function DeviceBrowser({
  open,
  onClose,
  onSelect,
  pickDirectory = false,
}: DeviceBrowserProps) {
  const [cwd, setCwd] = useState(ROOT_DIR);
  const [entries, setEntries] = useState<DeviceEntry[]>([]);
  /** Directory the current `entries` (or error) belong to — drives the loading state. */
  const [loadedDir, setLoadedDir] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);

  // Loading is derived: the list is stale/spinning whenever the current dir
  // does not yet match the dir the last (a)sync list call finished for.
  const isLoading = loadedDir !== cwd;

  // Reset navigation state whenever the modal opens. This is the official
  // "adjust state during render" pattern (React docs) — setState in an effect
  // body is flagged by react-hooks/set-state-in-effect.
  const [prevOpen, setPrevOpen] = useState(open);
  if (open !== prevOpen) {
    setPrevOpen(open);
    if (open) {
      setCwd(ROOT_DIR);
      setSelected(new Set());
    }
  }

  // Load the current directory whenever the modal opens or the dir changes.
  // All setState calls happen asynchronously (after the adb round-trip), so
  // the effect body itself stays side-effect-free; `cancelled` guards against
  // stale responses overwriting a newer navigation.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    (async () => {
      try {
        const list = await listDeviceDir(cwd);
        if (cancelled) return;
        setEntries(list);
        setLoadedDir(cwd);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        setEntries([]);
        setLoadedDir(cwd);
        const raw = err instanceof Error ? err.message : String(err);
        setError(`${USB_DEBUG_GUIDANCE}（${raw}）`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, cwd]);

  // Escape closes the modal.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  function navigateTo(dir: string) {
    setSelected(new Set());
    setCwd(dir);
  }

  function toggleFile(remotePath: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(remotePath)) next.delete(remotePath);
      else next.add(remotePath);
      return next;
    });
  }

  function handleConfirm() {
    onSelect(pickDirectory ? [cwd] : [...selected]);
  }

  const canConfirm = pickDirectory || selected.size > 0;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-8 bg-black/80">
      <div className="relative w-full max-w-2xl bg-zinc-900 rounded-xl overflow-hidden border border-zinc-800 shadow-2xl flex flex-col h-[70vh]">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-zinc-800 shrink-0">
          <h2 className="text-lg font-semibold text-zinc-100 flex items-center gap-2">
            <Smartphone className="h-5 w-5 text-indigo-400" />
            {pickDirectory ? "Choose device folder" : "Add from device"}
          </h2>
          <button onClick={onClose} className="p-1 text-zinc-400 hover:text-zinc-200">
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Current dir + up button */}
        <div className="flex items-center gap-2 px-4 pt-3 shrink-0">
          <button
            onClick={() => navigateTo(parentRemote(cwd))}
            disabled={cwd === "/"}
            className="p-1.5 rounded-md bg-zinc-800 hover:bg-zinc-700 text-zinc-300 disabled:opacity-40 disabled:hover:bg-zinc-800 transition-colors"
            title="Up one level"
            aria-label="Up one level"
          >
            <ArrowUp className="h-4 w-4" />
          </button>
          <span className="font-mono text-xs text-zinc-400 truncate">{cwd}</span>
        </div>

        {/* Common-dir shortcut chips */}
        <div className="flex items-center gap-1.5 px-4 pt-2 shrink-0">
          {COMMON_DIRS.map((d) => (
            <button
              key={d}
              onClick={() => navigateTo(joinRemote(ROOT_DIR, d))}
              className="px-2 py-0.5 rounded-md bg-zinc-800/60 hover:bg-zinc-700 text-[10px] text-zinc-400 hover:text-zinc-200 transition-colors"
            >
              {d}
            </button>
          ))}
        </div>

        {/* Body */}
        <div className="flex flex-col flex-1 min-h-0 px-4 py-3">
          {isLoading ? (
            <div className="flex items-center justify-center h-full gap-2 text-zinc-500 text-sm">
              <Loader2 className="h-4 w-4 animate-spin" />
              Loading…
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center gap-2 h-full text-center">
              <Smartphone className="h-8 w-8 text-amber-400/70" />
              <p className="text-sm text-zinc-300">{error}</p>
            </div>
          ) : (
            <ScrollArea className="flex-1 min-h-0 rounded-md border border-zinc-800/60">
              <div className="p-1">
                {entries.length === 0 ? (
                  <p className="text-center text-zinc-600 text-sm py-6">
                    (empty directory)
                  </p>
                ) : (
                    entries.map((e) => {
                      const remotePath = joinRemote(cwd, e.name);
                      const isChecked = selected.has(remotePath);
                      return e.isDir ? (
                        <button
                          key={remotePath}
                          onClick={() => navigateTo(remotePath)}
                          className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-zinc-800/70 text-left transition-colors"
                        >
                          <Folder className="h-4 w-4 text-indigo-400 shrink-0" />
                          <span className="text-sm text-zinc-200 truncate flex-1">
                            {e.name}
                          </span>
                          <span className="text-[10px] font-mono text-zinc-600 shrink-0">
                            dir
                          </span>
                        </button>
                      ) : (
                        <label
                          key={remotePath}
                          className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-zinc-800/70 cursor-pointer transition-colors"
                        >
                          <input
                            type="checkbox"
                            checked={isChecked}
                            onChange={() => toggleFile(remotePath)}
                            disabled={pickDirectory}
                            className="accent-indigo-500 h-4 w-4 shrink-0 disabled:opacity-30"
                          />
                          <File className="h-4 w-4 text-zinc-500 shrink-0" />
                          <span className="text-sm text-zinc-300 truncate flex-1">
                            {e.name}
                          </span>
                          <span className="text-[10px] font-mono text-zinc-600 shrink-0">
                            {formatBytesExact(e.size)}
                          </span>
                        </label>
                      );
                    })
                  )}
                </div>
              </ScrollArea>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-zinc-800 shrink-0">
          <span className="text-xs text-zinc-500">
            {pickDirectory
              ? "Navigate to the target folder, then confirm."
              : selected.size > 0
                ? `${selected.size} file${selected.size > 1 ? "s" : ""} selected`
                : "Select files, then confirm."}
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-3 py-1.5 rounded-md bg-zinc-800 hover:bg-zinc-700 text-zinc-300 text-sm transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleConfirm}
              disabled={!canConfirm || isLoading}
              className="px-3 py-1.5 rounded-md bg-indigo-600 hover:bg-indigo-500 text-white text-sm transition-colors disabled:opacity-40 disabled:hover:bg-indigo-600"
            >
              {pickDirectory
                ? "Select this folder"
                : selected.size > 0
                  ? `Add ${selected.size} file${selected.size > 1 ? "s" : ""}`
                  : "Add files"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
