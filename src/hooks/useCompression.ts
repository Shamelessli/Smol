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
  deliverToDevice,
  deleteLocalFile,
  type AdbProgressEvent,
} from "@/lib/tauri";
import type { CompressResult, VideoProgressEvent } from "@/lib/tauri";
import type { Job } from "@/types";
import { buildOutputPath, buildReplaceIntermediatePath } from "@/lib/outputPath";
import { extractErrorMessage } from "@/lib/errors";

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

      // Device jobs ALWAYS stage their output inside the import workspace
      // subdir (where the pulled input lives) — delivery happens afterwards,
      // regardless of the global outputMode. Branch first, before the local
      // replace/same-folder/custom logic.
      const isDeviceJob = !!job.deviceRemotePath;

      const outputPath = isDeviceJob
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

        // Device job: deliver the staged result to the device / PC folder,
        // then clean up the workspace copies.
        if (isDeviceJob) {
          await handleDeviceJob(jobId, job, result);
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

/**
 * Staging path for a device job's compressed output.
 * The pulled input lives at `{workspace}\{uuid}\{name}` (its dir IS the
 * workspace subdir), so the staged output sits next to it:
 * `{workspace}\{uuid}\{patternDerivedName}`.
 */
function stagingOutputPath(job: Job, pattern: string): string {
  const dir = job.inputPath.slice(
    0,
    Math.max(job.inputPath.lastIndexOf("\\"), job.inputPath.lastIndexOf("/")),
  );
  const dot = job.name.lastIndexOf(".");
  const stem = dot >= 0 ? job.name.slice(0, dot) : job.name;
  const ext = dot >= 0 ? job.name.slice(dot) : "";
  const name = pattern.replace("{name}", stem).replace("{ext}", ext);
  return `${dir}\\${name}`;
}

/**
 * Deliver a device job's compressed result according to its delivery mode,
 * then clean up the workspace copies:
 * - `outputLarger` → the device keeps the original; just drop the local copy.
 * - otherwise → `deliver_to_device` (replace / android-folder / pc-folder);
 *   on success delete both the staged output and the pulled input; on failure
 *   toast the real error (which may mention the recovered copy path).
 */
async function handleDeviceJob(
  jobId: string,
  job: Job,
  result: CompressResult,
): Promise<void> {
  const { customOutputDir } = useSettingsStore.getState();
  const staged = result.outputPath;

  if (result.outputLarger) {
    // Device keeps the original; remove BOTH local copies (input + staged output).
    useJobsStore
      .getState()
      .setJobOutput(jobId, job.deviceRemotePath ?? job.inputPath, result.outputBytes);
    await deleteLocalFile(job.inputPath).catch(() => {});
    await deleteLocalFile(result.outputPath).catch(() => {}); // the staged larger output
    return;
  }

  const mode = job.deviceDeliveryMode ?? "replace";
  const newName =
    staged.slice(Math.max(staged.lastIndexOf("\\"), staged.lastIndexOf("/")) + 1);
  const pcDir = job.devicePcFolder ?? customOutputDir ?? null;

  // Switch the progress bar from "compression" to the adb-push phase: once
  // the video/audio/image/pdf encoding bar has reached 100%, the green
  // "推送中…" bar takes over using the push channel's percent.
  useJobsStore.getState().setJobPushing(jobId, true);
  try {
    const pushChannel = new Channel<AdbProgressEvent>();
    pushChannel.onmessage = (ev) => {
      useJobsStore.getState().updateJobProgress(jobId, { progress: ev.percent });
    };
    const deliver = await deliverToDevice(
      staged,
      mode,
      job.deviceRemotePath!,
      job.deviceRemoteDir ?? null,
      pcDir,
      newName,
      pushChannel,
    );
    if (deliver.note) toast.info(deliver.note);
    useJobsStore.getState().setJobOutput(jobId, deliver.path ?? staged, result.outputBytes);
    await deleteLocalFile(staged).catch(() => {});
    await deleteLocalFile(job.inputPath).catch(() => {});
  } catch (err) {
    // The Rust command may have preserved the compressed file and reported
    // where — surface that real message instead of a generic one.
    toast.error(extractErrorMessage(err) || "已压缩，但交付失败", { duration: 6000 });
    useJobsStore.getState().setJobOutput(jobId, staged, result.outputBytes);
  } finally {
    useJobsStore.getState().setJobPushing(jobId, false);
  }
}
