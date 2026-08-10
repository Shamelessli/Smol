/**
 * Compute the output file path from a job's input path and the current
 * output-mode / filename-pattern settings.
 *
 * Works on Windows paths (backslash) and POSIX paths (forward slash).
 */
export function buildOutputPath(
  inputPath: string,
  // Accept the full Settings.outputMode union so callers can pass it through;
  // "replace" is not a real buildOutputPath mode — it falls through to the
  // same-folder default and is handled via buildReplaceIntermediatePath().
  outputMode: "same-folder" | "subfolder" | "custom" | "replace",
  filenamePattern: string,
  customOutputDir?: string,
): string {
  // Detect path separator (Windows-first since that's the only v1.0 target)
  const sep = inputPath.includes("\\") ? "\\" : "/";
  const lastSep = Math.max(inputPath.lastIndexOf("\\"), inputPath.lastIndexOf("/"));

  const dir = inputPath.slice(0, lastSep);
  const filename = inputPath.slice(lastSep + 1);

  const dotIdx = filename.lastIndexOf(".");
  const name = dotIdx >= 0 ? filename.slice(0, dotIdx) : filename;
  const ext = dotIdx >= 0 ? filename.slice(dotIdx) : ""; // e.g. ".mp4"

  const outFilename = filenamePattern
    .replace("{name}", name)
    .replace("{ext}", ext);

  let outDir: string;
  switch (outputMode) {
    case "subfolder":
      outDir = `${dir}${sep}smol`;
      break;
    case "custom":
      outDir = customOutputDir ?? dir;
      break;
    case "same-folder":
    default:
      outDir = dir;
  }

  return `${outDir}${sep}${outFilename}`;
}

/**
 * Compute the transient intermediate output path for "replace original" mode.
 * Always sits in the input's directory with a distinct name (`{name}_smol{ext}`)
 * so the compression commands never see input == output.
 */
export function buildReplaceIntermediatePath(inputPath: string): string {
  const sep = inputPath.includes("\\") ? "\\" : "/";
  const lastSep = Math.max(inputPath.lastIndexOf("\\"), inputPath.lastIndexOf("/"));
  const dir = inputPath.slice(0, lastSep);
  const filename = inputPath.slice(lastSep + 1);
  const dotIdx = filename.lastIndexOf(".");
  const name = dotIdx >= 0 ? filename.slice(0, dotIdx) : filename;
  const ext = dotIdx >= 0 ? filename.slice(dotIdx) : "";
  return `${dir}${sep}${name}_smol${ext}`;
}
