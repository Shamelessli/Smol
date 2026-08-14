import { Channel } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { useJobsStore } from "@/store/jobs";
import { useSettingsStore } from "@/store/settings";
import {
  compressAudio,
  compressImage,
  compressPdf,
  compressVideo,
  replaceOriginal,
  deliverOutput,
  deleteLocalFile,
} from "@/lib/tauri";
import type { VideoProgressEvent } from "@/lib/tauri";
import { buildOutputPath, buildReplaceIntermediatePath } from "@/lib/outputPath";

/**
 * Extract a human-readable string from whatever Tauri throws on command failure.
 *
 * Tauri rejects with a serialised AppError object: { kind: "Other", message: "…" }
 * rather than a JS Error instance, so we probe for `.message` first.
 */
function extractErrorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (
    err !== null &&
    typeof err === "object" &&
    "message" in err &&
    typeof (err as Record<string, unknown>).message === "string"
  ) {
    return (err as Record<string, unknown>).message as string;
  }
  return String(err);
}

/**
 * Start compression for every "ready" video or audio job in the queue.
 *
 * All eligible jobs are fired concurrently.  Each gets its own Channel so
 * progress events are routed atomically to the correct row — no extra UI code
 * needed for audio because it reuses the identical state machine.
 *
 * Audio note: the Rust command may change the output file extension
 * (e.g. WAV → mp3).  CompressResult.outputPath always reflects the actual
 * file written on disk, so the store is always consistent.
 */
export async function startSqueeze(): Promise<void> {
  const { jobs, jobIds } = useJobsStore.getState();
  const { preset, outputMode, filenamePattern, customOutputDir } =
    useSettingsStore.getState();

  // Collect ready video + audio + image + pdf jobs
  const readyIds = jobIds.filter(
    (id) =>
      (jobs[id]?.kind === "video" ||
        jobs[id]?.kind === "audio" ||
        jobs[id]?.kind === "image" ||
        jobs[id]?.kind === "pdf") &&
      jobs[id]?.status === "ready",
  );

  if (readyIds.length === 0) return;

  // Transition all to "encoding" before spawning so the UI reacts immediately
  for (const id of readyIds) {
    useJobsStore.getState().transitionStatus(id, "encoding");
  }

  const parallelLimit = useSettingsStore.getState().parallelJobs || 4;
  const executing = new Set<Promise<void>>();

  for (const jobId of readyIds) {
    const p = (async () => {
      const job = useJobsStore.getState().jobs[jobId];
      if (!job) return;

      const outputPath =
        job.imported
          ? stagingOutputPath(job, filenamePattern)
          : outputMode === "replace"
            ? buildReplaceIntermediatePath(job.inputPath)
            : buildOutputPath(
                job.inputPath,
                outputMode,
                filenamePattern,
                customOutputDir,
              );

      // Each job gets its own channel — events carry jobId so routing is exact
      const channel = new Channel<VideoProgressEvent>();
      channel.onmessage = (ev) => {
        useJobsStore.getState().updateJobProgress(jobId, {
          progress: Math.round(ev.fraction * 100),
          speed: ev.speed,
          etaSec: ev.etaSec,
          outputBytes: ev.currentBytes,
        });
      };

      try {
        let result;
        if (job.kind === "image") {
          result = await compressImage(
            jobId,
            job.inputPath,
            outputPath,
            preset,
            job.probe?.durationSec ?? null,
            channel,
          );
        } else {
          const compressFn =
            job.kind === "audio" ? compressAudio :
            job.kind === "pdf"   ? compressPdf   :
            compressVideo;
            
          const targetFileSize = job.kind === "video" 
            ? (useSettingsStore.getState().targetFileSize?.value ?? null)
            : null;

          result = await (compressFn as unknown as CallableFunction)(
            jobId,
            job.inputPath,
            outputPath,
            preset,
            job.probe?.durationSec ?? null,
            channel,
            targetFileSize
          );
        }

        // Replace mode: move original to Recycle Bin, put compressed in its place.
        // outputLarger (compressed ≥ original) → original kept, nothing replaced.
        if (job.imported) {
          await handleImportedJob(jobId, job, result, outputMode);
        } else if (outputMode === "replace" && !result.outputLarger) {
          const finalPath = await replaceOriginal(result.outputPath, job.inputPath);
          useJobsStore.getState().setJobOutput(jobId, finalPath, result.outputBytes, true);
        } else {
          useJobsStore.getState().setJobOutput(jobId, result.outputPath, result.outputBytes);
        }
      } catch (err) {
        useJobsStore.getState().setJobError(jobId, extractErrorMessage(err));
      }
    })().finally(() => executing.delete(p));

    executing.add(p);
    if (executing.size >= parallelLimit) {
      await Promise.race(executing);
    }
  }

  await Promise.all(executing);
}

/** Staging path for an imported job's output, inside its workspace subdir. */
function stagingOutputPath(job: import("@/types").Job, pattern: string): string {
  const dir = job.inputPath.slice(0, Math.max(job.inputPath.lastIndexOf("\\"), job.inputPath.lastIndexOf("/")));
  const dot = job.name.lastIndexOf(".");
  const stem = dot >= 0 ? job.name.slice(0, dot) : job.name;
  const ext  = dot >= 0 ? job.name.slice(dot) : "";
  const name = pattern.replace("{name}", stem).replace("{ext}", ext);
  return `${dir}\\${name}`;
}

/** Deliver an imported (device) job's result to the device, then clean up. */
async function handleImportedJob(
  jobId: string,
  job: import("@/types").Job,
  result: { outputPath: string; outputBytes: number; outputLarger: boolean },
  outputMode: "same-folder" | "subfolder" | "custom" | "replace",
): Promise<void> {
  const { customOutputDir } = useSettingsStore.getState();
  const staged = result.outputPath;

  if (result.outputLarger) {
    // already optimal: device keeps the original, drop the local copy
    useJobsStore.getState().setJobOutput(jobId, job.inputPath, result.outputBytes);
    await deleteLocalFile(job.inputPath).catch(() => {});
    return;
  }

  const key = job.inputPath.slice(0, Math.max(job.inputPath.lastIndexOf("\\"), job.inputPath.lastIndexOf("/")))
    .split(/[\\/]/).pop() ?? "";
  const originalName = job.name;
  const newName = staged.slice(Math.max(staged.lastIndexOf("\\"), staged.lastIndexOf("/")) + 1);

  try {
    const deliver = await deliverOutput(
      staged,
      key,
      outputMode,
      customOutputDir ?? null,
      newName,
      originalName,
      job.importParentIdListB64!,
    );
    if (deliver.note) toast.info(deliver.note);
    useJobsStore.getState().setJobOutput(jobId, staged, result.outputBytes);
    await deleteLocalFile(staged).catch(() => {});
    await deleteLocalFile(job.inputPath).catch(() => {});
  } catch {
    toast.error("已压缩，但写回设备失败，结果保存在本地", { duration: 6000 });
    useJobsStore.getState().setJobOutput(jobId, staged, result.outputBytes);
  }
}
