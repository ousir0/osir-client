import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  artifactDownloadTimeout,
  downloadReleaseArtifact,
} from "./osir-release-download.mjs";

function artifact(data) {
  return {
    url: "https://osirclaw.com/api/v1/osir-client/artifacts/prod-0.1.3/client.exe",
    size_bytes: data.length,
    sha256: createHash("sha256").update(data).digest("hex"),
  };
}

function response(url, data) {
  const value = new Response(data);
  Object.defineProperty(value, "url", { value: url });
  return value;
}

test("artifact downloads stream to a sink and report verified completion", async () => {
  const data = Buffer.from("verified release bytes");
  const item = artifact(data),
    chunks = [],
    progress = [];
  await downloadReleaseArtifact(item, {
    fetchImpl: async (url) => response(url, data),
    onChunk: (chunk) => chunks.push(Buffer.from(chunk)),
    onProgress: (event) => progress.push(event.phase),
  });
  assert.deepEqual(Buffer.concat(chunks), data);
  assert.deepEqual(progress, ["start", "complete"]);
});

test("artifact verification rejects changed bytes, size and redirects", async () => {
  const expected = artifact(Buffer.from("right"));
  await assert.rejects(
    downloadReleaseArtifact(expected, {
      fetchImpl: async (url) => response(url, Buffer.from("wrong")),
      onProgress() {},
    }),
    /hash differs/,
  );
  await assert.rejects(
    downloadReleaseArtifact(expected, {
      fetchImpl: async (url) => response(url, Buffer.from("too long")),
      onProgress() {},
    }),
    /size differs/,
  );
  await assert.rejects(
    downloadReleaseArtifact(expected, {
      fetchImpl: async (url) =>
        response(url + "?redirected", Buffer.from("right")),
      onProgress() {},
    }),
    /failed or redirected/,
  );
});

test("artifact download aborts on a stalled body and enforces a total limit", async () => {
  const item = artifact(Buffer.from("x"));
  const hangingFetch = async (_url, { signal }) => ({
    status: 200,
    url: item.url,
    headers: { get: () => null },
    body: {
      async *[Symbol.asyncIterator]() {
        await new Promise((_, reject) => {
          signal.addEventListener("abort", () => reject(signal.reason), {
            once: true,
          });
        });
      },
    },
  });
  await assert.rejects(
    downloadReleaseArtifact(item, {
      fetchImpl: hangingFetch,
      idleMs: 10,
      totalMs: 1000,
      onProgress() {},
    }),
    /stalled/,
  );
  await assert.rejects(
    downloadReleaseArtifact(item, {
      fetchImpl: hangingFetch,
      idleMs: 1000,
      totalMs: 10,
      onProgress() {},
    }),
    /time limit exceeded/,
  );
  assert.ok(artifactDownloadTimeout(231_645_062) > 180_000);
  assert.equal(artifactDownloadTimeout(2 * 1024 ** 3), 45 * 60_000);
});

test("artifact readback follows only its exact RainS3 object and still checks bytes", async () => {
  const data = Buffer.from("object release bytes");
  const item = artifact(data);
  const target =
    "https://osirvedio.cn-nb1.rains3.com/osir-client/releases/prod-0.1.3/client.exe";
  const requests = [];
  const redirected = (location) => {
    const value = new Response(null, { status: 302, headers: { location } });
    Object.defineProperty(value, "url", { value: item.url });
    return value;
  };
  await downloadReleaseArtifact(item, {
    fetchImpl: async (url, options) => {
      requests.push([url, options.redirect]);
      return url === item.url ? redirected(target) : response(url, data);
    },
    onProgress() {},
  });
  assert.deepEqual(requests, [
    [item.url, "manual"],
    [target, "error"],
  ]);
  for (const destination of [
    "https://evil.test/client.exe",
    target + "?token=x",
    target.replace("prod-0.1.3", "prod-0.1.4"),
  ]) {
    await assert.rejects(
      downloadReleaseArtifact(item, {
        fetchImpl: async () => redirected(destination),
        onProgress() {},
      }),
      /destination rejected/,
    );
  }
  await assert.rejects(
    downloadReleaseArtifact(item, {
      fetchImpl: async (url) =>
        url === item.url
          ? redirected(target)
          : response(url, Buffer.from("wrong release bytes!")),
      onProgress() {},
    }),
    /size differs|hash differs/,
  );
});
