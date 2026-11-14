import { createHash } from "node:crypto";

export function artifactDownloadTimeout(size) {
  if (!Number.isSafeInteger(size) || size <= 0 || size > 2 * 1024 ** 3)
    throw new Error("Invalid release artifact size.");
  return Math.min(
    45 * 60000,
    5 * 60000 + Math.ceil(size / (256 * 1024)) * 1000,
  );
}

export function reportArtifactProgress(event) {
  process.stderr.write(
    `OSIR artifact ${event.phase}: ${event.name} ${event.bytes}/${event.total} bytes (${event.elapsed_seconds}s)\n`,
  );
}

// Total time scales with package size; idle time bounds a stalled connection.
export async function downloadReleaseArtifact(
  artifact,
  {
    fetchImpl = globalThis.fetch,
    onChunk = () => {},
    onProgress = reportArtifactProgress,
    idleMs = 90000,
    totalMs = artifactDownloadTimeout(artifact.size_bytes),
  } = {},
) {
  const controller = new AbortController();
  const started = Date.now();
  const name = new URL(artifact.url).pathname.split("/").at(-1);
  let bytes = 0,
    idleTimer,
    lastProgress = started;
  const emit = (phase) =>
    onProgress({
      phase,
      name,
      bytes,
      total: artifact.size_bytes,
      elapsed_seconds: Math.round((Date.now() - started) / 1000),
    });
  const abort = (reason) =>
    controller.abort(new Error(`Artifact download ${reason}: ${name}`));
  const resetIdle = () => {
    clearTimeout(idleTimer);
    idleTimer = setTimeout(() => abort("stalled"), idleMs);
  };
  const totalTimer = setTimeout(() => abort("time limit exceeded"), totalMs);
  resetIdle();
  try {
    emit("start");
    const requestOptions = {
      method: "GET",
      redirect: "manual",
      signal: controller.signal,
      headers: {
        accept: "application/octet-stream",
        "accept-encoding": "identity",
      },
    };
    let response = await fetchImpl(artifact.url, requestOptions);
    let finalURL = artifact.url;
    if (response.status === 302) {
      const prefix = "https://osirclaw.com/api/v1/osir-client/artifacts/";
      const relative = artifact.url.startsWith(prefix)
        ? artifact.url.slice(prefix.length)
        : "";
      if (
        !/^[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(
          relative,
        ) ||
        relative.includes("..")
      )
        throw new Error(`Public artifact redirect identity rejected: ${name}`);
      finalURL = `https://osirvedio.cn-nb1.rains3.com/osir-client/releases/${relative}`;
      if (
        response.url !== artifact.url ||
        response.headers.get("location") !== finalURL
      )
        throw new Error(
          `Public artifact redirect destination rejected: ${name}`,
        );
      await response.body?.cancel();
      resetIdle();
      response = await fetchImpl(finalURL, {
        ...requestOptions,
        redirect: "error",
      });
    }
    if (response.status !== 200 || response.url !== finalURL || !response.body)
      throw new Error(`Public artifact download failed or redirected: ${name}`);
    const length = response.headers?.get("content-length");
    if (length != null && Number(length) !== artifact.size_bytes)
      throw new Error(`Public artifact response size differs: ${name}`);
    const hash = createHash("sha256");
    for await (const chunk of response.body) {
      controller.signal.throwIfAborted();
      resetIdle();
      bytes += chunk.length;
      if (bytes > artifact.size_bytes)
        throw new Error(`Public artifact size differs: ${name}`);
      hash.update(chunk);
      onChunk(chunk);
      if (Date.now() - lastProgress >= 30000) {
        emit("progress");
        lastProgress = Date.now();
      }
    }
    controller.signal.throwIfAborted();
    if (bytes !== artifact.size_bytes || hash.digest("hex") !== artifact.sha256)
      throw new Error(`Public artifact hash differs: ${name}`);
    emit("complete");
  } finally {
    clearTimeout(totalTimer);
    clearTimeout(idleTimer);
    controller.abort();
  }
}
