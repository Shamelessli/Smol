import { useState } from "react";
import { Channel } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { v4 as uuidv4 } from "uuid";
import { Upload, Trash2, Smartphone, Loader2 } from "lucide-react";
import { toast } from "sonner";
import { motion, AnimatePresence } from "framer-motion";
import { cn } from "@/lib/utils";
import { fileKindFromPath } from "@/lib/kinds";
import { extractErrorMessage } from "@/lib/errors";
import {
  getPathInfo,
  getImportWorkspace,
  pullDeviceFiles,
  type AdbProgressEvent,
} from "@/lib/tauri";
import { useJobsStore } from "@/store/jobs";
import type { NewJobInput } from "@/store/jobs";
import { DeviceBrowser } from "@/components/device/DeviceBrowser";
import { EmptyState } from "./EmptyState";

const VIDEO_EXTS = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "wmv", "flv"];
const AUDIO_EXTS = ["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "wma"];
const IMAGE_EXTS = ["jpg", "jpeg", "png", "webp", "heic", "heif", "avif", "bmp", "tiff"];
const PDF_EXTS   = ["pdf"];
const ALL_EXTS   = [...VIDEO_EXTS, ...AUDIO_EXTS, ...IMAGE_EXTS, ...PDF_EXTS];

interface DropzoneProps {
  isDraggingOver: boolean;
  hasFiles: boolean;
}

export function Dropzone({ isDraggingOver, hasFiles }: DropzoneProps) {
  const [browserOpen, setBrowserOpen] = useState(false);
  const [pulling, setPulling] = useState<{ file: string; percent: number } | null>(null);

  async function handleOpenDialog() {
    const selected = await open({
      multiple: true,
      filters: [
        { name: "All supported", extensions: ALL_EXTS },
        { name: "Video",         extensions: VIDEO_EXTS },
        { name: "Audio",         extensions: AUDIO_EXTS },
        { name: "Images",        extensions: IMAGE_EXTS },
        { name: "PDF",           extensions: PDF_EXTS   },
      ],
    });

    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];

    const toAdd: NewJobInput[] = [];

    for (const path of paths) {
      const info = await getPathInfo(path);
      if (!info.exists) continue;
      const kind = fileKindFromPath(info.name);
      if (kind === "unsupported") continue;
      toAdd.push({ id: uuidv4(), inputPath: info.path, name: info.name, kind, inputBytes: info.size });
    }
    if (toAdd.length > 0) {
      useJobsStore.getState().addFiles(toAdd);
    }
  }

  /** Pull the selected remote files into the import workspace and enqueue them. */
  async function handleDeviceSelect(paths: string[]) {
    if (paths.length === 0) return;
    const channel = new Channel<AdbProgressEvent>();
    channel.onmessage = (ev) => {
      const short = ev.file.split("/").pop() ?? ev.file;
      setPulling({ file: short, percent: ev.percent });
    };
    setPulling({
      file: paths.length > 1 ? `${paths.length} 个文件` : (paths[0].split("/").pop() ?? paths[0]),
      percent: 0,
    });
    try {
      const workspace = await getImportWorkspace();
      const results = await pullDeviceFiles(paths, workspace, channel);

      const ok = results.filter((r) => r.ok);
      const failed = results.filter((r) => !r.ok);

      const toAdd: NewJobInput[] = [];
      for (const r of ok) {
        const kind = fileKindFromPath(r.name);
        if (kind === "unsupported") continue;
        toAdd.push({
          id: uuidv4(),
          inputPath: r.localPath,
          name: r.name,
          kind,
          inputBytes: r.size,
          deviceRemotePath: r.remotePath,
          deviceDeliveryMode: "replace",
        });
      }

      if (failed.length > 0) {
        const detail = failed
          .map((f) => f.error)
          .filter(Boolean)
          .join("；");
        toast.error(
          `${failed.length} 个文件拉取失败。${detail || "请检查设备连接后重试"}`,
          { duration: 8000 },
        );
      }

      if (toAdd.length > 0) {
        useJobsStore.getState().addFiles(toAdd);
        toast.success(`Imported ${toAdd.length} file${toAdd.length > 1 ? "s" : ""} from device`);
      } else if (failed.length === 0) {
        toast("No supported files selected on device");
      }
    } catch (err) {
      toast.error(extractErrorMessage(err), { duration: 6000 });
    } finally {
      setPulling(null);
    }
  }

  function handleClearAll() {
    const currentState = useJobsStore.getState();
    const prevJobs = { ...currentState.jobs };
    const prevJobIds = [...currentState.jobIds];
    const count = prevJobIds.length;
    
    currentState.clear();
    
    toast(`Cleared ${count} files`, {
      action: {
        label: "Undo",
        onClick: () => {
          useJobsStore.setState({ jobs: prevJobs, jobIds: prevJobIds });
        },
      },
      duration: 4000,
    });
  }

  // Outer motion.div: flex-1 when empty (fills container), h-12 when compact.
  // The `layout` prop makes Framer Motion animate the height change (~200 ms easeOut).
  return (
    <>
    <motion.div
      layout
      transition={{ duration: 0.2, ease: "easeOut" }}
      className={cn(
        "overflow-hidden",
        !hasFiles
          ? "flex flex-col flex-1 m-3"
          : "shrink-0 h-12 mx-3 mt-3"
      )}
    >
      <AnimatePresence initial={false} mode="popLayout">
        {!hasFiles ? (
          // ── Empty state ────────────────────────────────────────────────────
          <motion.div
            key="empty"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15 }}
            className="flex flex-col flex-1"
          >
            {/* whileHover: spring scale on the dashed panel (empty state only) */}
            <motion.div
              whileHover={{ scale: 1.01 }}
              transition={{ type: "spring", stiffness: 400, damping: 25 }}
              className={cn(
                "flex flex-col flex-1 items-center justify-center gap-4 min-h-[260px]",
                "rounded-xl border-2 border-dashed transition-colors",
                isDraggingOver
                  ? "border-indigo-500 bg-indigo-950/30 shadow-lg shadow-indigo-500/20"
                  : "border-zinc-700 hover:border-zinc-500"
              )}
            >
              <motion.div
                animate={{ scale: isDraggingOver ? 1.2 : 1, y: isDraggingOver ? -5 : 0 }}
                transition={{ type: "spring", stiffness: 400, damping: 25 }}
                className="text-zinc-600 mb-2"
              >
                <Upload className="h-10 w-10 opacity-50" />
              </motion.div>
              <EmptyState isDraggingOver={isDraggingOver} />
              <div className="flex items-center gap-4">
                <button
                  onClick={handleOpenDialog}
                  className="flex items-center gap-2 px-4 py-2 rounded-lg bg-zinc-800 hover:bg-zinc-700 text-zinc-300 text-sm transition-colors"
                >
                  <Upload className="h-4 w-4" />
                  Open files…
                </button>
                <button
                  onClick={() => setBrowserOpen(true)}
                  className="flex items-center gap-2 px-4 py-2 rounded-lg bg-zinc-800 hover:bg-zinc-700 text-zinc-300 text-sm transition-colors"
                >
                  <Smartphone className="h-4 w-4" />
                  Add from device
                </button>
              </div>
            </motion.div>
          </motion.div>
        ) : (
          // ── Compact ~48 px toolbar ─────────────────────────────────────────
          <motion.div
            key="compact"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15 }}
            className={cn(
              "flex items-center justify-between h-full px-3 rounded-xl border-2 transition-colors",
              isDraggingOver
                ? "border-indigo-500 bg-indigo-950/30 shadow-[0_0_15px_rgba(99,102,241,0.2)]"
                : "border-transparent bg-zinc-900/50"
            )}
          >
            <span className="text-zinc-500 text-xs transition-colors flex items-center gap-2">
              <motion.div animate={{ y: isDraggingOver ? -2 : 0, opacity: isDraggingOver ? 1 : 0.5 }}>
                <Upload className="h-4 w-4" />
              </motion.div>
              {isDraggingOver ? <span className="text-indigo-400">Release to add…</span> : "Drop files anywhere to add"}
            </span>
            <div className="flex items-center gap-2">
              <button
                onClick={handleClearAll}
                className="flex items-center gap-1.5 px-3 py-1 rounded-md text-zinc-500 hover:text-zinc-300 hover:bg-zinc-800 text-xs transition-colors"
                title="Clear all files from queue"
              >
                <Trash2 className="h-3.5 w-3.5" />
                Clear all
              </button>
              <button
                onClick={handleOpenDialog}
                className="flex items-center gap-1.5 px-3 py-1 rounded-md bg-zinc-800 hover:bg-zinc-700 text-zinc-300 text-xs transition-colors"
              >
                <Upload className="h-3.5 w-3.5" />
                Add more…
              </button>
              <button
                onClick={() => setBrowserOpen(true)}
                className="flex items-center gap-1.5 px-3 py-1 rounded-md bg-zinc-800 hover:bg-zinc-700 text-zinc-300 text-xs transition-colors"
                title="Add files from an Android device"
              >
                <Smartphone className="h-3.5 w-3.5" />
                Add from device
              </button>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>

    {/* Device file browser modal */}
    <DeviceBrowser
      open={browserOpen}
      onClose={() => setBrowserOpen(false)}
      onSelect={(paths) => {
        setBrowserOpen(false);
        void handleDeviceSelect(paths);
      }}
    />

    {/* Pull progress bar — emerald to visually echo the device-push phase so the
        user reads "device transfer" consistently, distinct from compression's
        indigo/purple. */}
    {pulling && (
      <div className="fixed bottom-4 left-1/2 -translate-x-1/2 z-50 w-[420px] max-w-[90vw] px-4 py-3 rounded-lg bg-zinc-900/95 border border-emerald-700/40 shadow-2xl">
        <div className="flex items-center justify-between text-xs text-zinc-300 mb-1.5">
          <span className="flex items-center gap-1.5 truncate">
            <Loader2 className="h-3.5 w-3.5 animate-spin shrink-0 text-emerald-400" />
            <span className="truncate">正在从设备拉取：{pulling.file}</span>
          </span>
          <span className="font-mono tabular-nums shrink-0 text-emerald-400">{pulling.percent}%</span>
        </div>
        <div className="h-1.5 rounded-full bg-zinc-800 overflow-hidden">
          <div
            className="h-full bg-gradient-to-r from-emerald-500 to-teal-500 transition-[width] duration-200 ease-out"
            style={{ width: `${pulling.percent}%` }}
          />
        </div>
      </div>
    )}
    </>
  );
}
