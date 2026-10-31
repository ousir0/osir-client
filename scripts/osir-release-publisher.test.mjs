import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import {
  createReleasePlan,
  inspectArtifact,
  runReleaseAction,
  createServiceReleaseMetadata,
  verifyPublicReleaseArtifacts,
} from "./osir-release-publisher.mjs";

// Public minisign-verify 0.2.5 vector; no private key or actual signing.
test("public verification deduplicates URLs but rejects conflicting identities before fetching", async () => {
  const artifact = {
    url: "https://osirclaw.com/test",
    sha256: createHash("sha256").update("test").digest("hex"),
    size_bytes: 4,
  };
  let calls = 0;
  const fetchImpl = async () => {
    calls++;
    return {
      status: 200,
      ok: true,
      url: artifact.url,
      headers: { get: () => null },
      body: [Buffer.from("test")],
    };
  };
  const receipt = await verifyPublicReleaseArtifacts(
    { release_id: "test", artifacts: [artifact, { ...artifact }] },
    { fetchImpl },
  );
  assert.equal(calls, 1);
  assert.equal(receipt.artifacts.length, 2);
  calls = 0;
  await assert.rejects(
    verifyPublicReleaseArtifacts(
      { artifacts: [artifact, { ...artifact, size_bytes: 5 }] },
      { fetchImpl },
    ),
    /Conflicting/,
  );
  assert.equal(calls, 0);
});
const pubkey = Buffer.from(
  "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n",
).toString("base64");
const signature = Buffer.from(
  "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n",
).toString("base64");
const digest = (value) => createHash("sha256").update(value).digest("hex");
function fixture(t, universal = false) {
  const root = mkdtempSync(join(tmpdir(), "osir-publisher-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const channel = "beta",
    version = "4.0.21-beta.1";
  const target = universal ? "universal-apple-darwin" : "aarch64-apple-darwin";
  const name = `OSIR-Client-${channel}-${version}-${target}`;
  const config = {
    schemaVersion: 1,
    channel,
    pubkey,
    endpoint: `https://osirclaw.com/api/v1/osir-client/updater/${channel}.json`,
    downloadBaseUrl: "https://osirclaw.com/client-release/",
    notes: "Test release",
  };
  const configPath = join(root, "public.json");
  writeFileSync(configPath, JSON.stringify(config));
  writeFileSync(join(root, name + ".app.tar.gz"), "test");
  writeFileSync(join(root, name + ".app.tar.gz.sig"), signature + "\n");
  writeFileSync(join(root, name + ".zip"), "synthetic installer");
  const entry = {
    signature,
    url: `${config.downloadBaseUrl}${channel}/${version}/${name}.app.tar.gz`,
  };
  const feed = {
    version,
    notes: config.notes,
    pub_date: "2026-10-29T10:00:00Z",
    platforms: {
      "darwin-aarch64": entry,
      ...(universal ? { "darwin-x86_64": entry } : {}),
    },
  };
  writeFileSync(join(root, "updater-beta.json"), JSON.stringify(feed));
  const manifest = {
    channel,
    version,
    mode: "release-distribution",
    architectures: universal ? ["arm64", "x86_64"] : ["arm64"],
    distributionSignatureVerified: true,
    signing: "developer-id-notarized",
    updaterEnabled: true,
    updaterRuntimeConfigured: true,
    updaterUpgradeAcceptance: "not-run",
    updaterConfigSha256: digest(JSON.stringify(config)),
    updaterArtifact: {
      sha256: digest("test"),
      bytes: 4,
      signatureVerified: true,
    },
    installerArtifact: {
      name: name + ".zip",
      sha256: digest("synthetic installer"),
      bytes: 19,
    },
    sourceProvenance: {
      verifiedAtEnd: true,
      sourceSha256: "a".repeat(64),
      git: { head: "b".repeat(40), dirtyAtStart: true },
    },
  };
  writeFileSync(join(root, "build-manifest.json"), JSON.stringify(manifest));
  return {
    root,
    manifest,
    feed,
    config,
    configPath,
    name,
    options: { channel, buildDirectory: root, updaterConfigPath: configPath },
  };
}
function reply(data, status = 200, url = "") {
  const r = new Response(
    status === 204 ? null : JSON.stringify({ code: 0, data }),
    { status },
  );
  Object.defineProperty(r, "url", { value: url });
  return r;
}
function artifactReply(artifact, corrupt = false) {
  const r = new Response(corrupt ? "corrupt" : readFileSync(artifact.path));
  Object.defineProperty(r, "url", { value: artifact.url });
  return r;
}
function publicDraft(plan) {
  return {
    ...plan,
    status: "draft",
    artifacts: plan.artifacts.map(
      ({ platform, kind, url, sha256, size_bytes, signature }) => ({
        platform,
        kind,
        url,
        sha256,
        size_bytes,
        ...(signature ? { signature } : {}),
      }),
    ),
  };
}
const token = "synthetic-admin-token-for-test-only";

test("public verification receipt binds raw service metadata and full artifact bytes without credentials", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options);
  const metadata = createServiceReleaseMetadata(plan);
  assert.equal(Object.hasOwn(metadata, "distribution_signing"), false);
  const path = join(f.root, "metadata.json");
  const raw = JSON.stringify(metadata, null, 2) + "\n";
  writeFileSync(path, raw);
  const fetchImpl = async (url, options) => {
    assert.equal(options.headers.authorization, undefined);
    assert.equal(options.redirect, "error");
    return artifactReply(plan.artifacts.find((item) => item.url === url));
  };
  const receipt = await verifyPublicReleaseArtifacts(plan, {
    metadataPath: path,
    fetchImpl,
  });
  assert.equal(receipt.metadata_sha256, digest(raw));
  assert.equal(receipt.updater_signature_verified, true);
  assert.equal(receipt.artifacts.length, 2);
  assert.ok(receipt.artifacts.every((item) => item.verified));
  await assert.rejects(
    verifyPublicReleaseArtifacts(plan, {
      metadataPath: path,
      fetchImpl: async (url) =>
        artifactReply(
          plan.artifacts.find((item) => item.url === url),
          true,
        ),
    }),
    /hash differs|size differs/,
  );
  metadata.notes = "tampered";
  writeFileSync(path, JSON.stringify(metadata));
  await assert.rejects(
    verifyPublicReleaseArtifacts(plan, { metadataPath: path, fetchImpl }),
    /metadata differs/,
  );
});

test("explicit ad-hoc distribution uses DMG and reports absence of Apple notarization", (t) => {
  const f = fixture(t);
  f.manifest.mode = "release-adhoc-distribution";
  f.manifest.signing = "ad-hoc-not-notarized";
  f.manifest.distributionSignatureVerified = false;
  f.manifest.codeSignatureVerified = true;
  f.manifest.notarization = "not-notarized";
  const content = "synthetic dmg";
  writeFileSync(join(f.root, f.name + ".dmg"), content);
  f.manifest.installerArtifact = {
    name: f.name + ".dmg",
    sha256: digest(content),
    bytes: Buffer.byteLength(content),
  };
  const save = () =>
    writeFileSync(
      join(f.root, "build-manifest.json"),
      JSON.stringify(f.manifest),
    );
  save();
  const plan = createReleasePlan(f.options);
  assert.equal(plan.distribution_signature_verified, false);
  assert.equal(plan.code_signature_verified, true);
  assert.equal(plan.updater_signature_verified, true);
  assert.equal(plan.notarization, "not-notarized");
  assert.equal(
    plan.artifacts.find((item) => item.kind === "installer").file_name,
    f.name + ".dmg",
  );
  assert.match(plan.notes, /未经 Apple 公证/);
  for (const key of [
    "codeSignatureVerified",
    "distributionSignatureVerified",
    "notarization",
  ]) {
    const original = f.manifest[key];
    f.manifest[key] = key === "notarization" ? "stapled-verified" : !original;
    save();
    assert.throws(() => createReleasePlan(f.options));
    f.manifest[key] = original;
  }
  f.manifest.installerArtifact.name = f.name + ".zip";
  save();
  assert.throws(() => createReleasePlan(f.options), /requires a DMG/);
});

test("plan binds signed updater, installer digest, source snapshot and immutable URLs", (t) => {
  const f = fixture(t);
  const p = createReleasePlan(f.options);
  assert.equal(p.release_id, "beta-4.0.21-beta.1");
  assert.equal(p.source_sha, "a".repeat(64));
  assert.equal(p.updater_acceptance, "not-run");
  assert.equal(p.artifacts.length, 2);
  assert.ok(
    p.artifacts.every((a) =>
      a.url.startsWith(
        "https://osirclaw.com/api/v1/osir-client/artifacts/beta-4.0.21-beta.1/",
      ),
    ),
  );
});

test("stable local certificate release preserves non-notarized distribution boundaries", (t) => {
  const f = fixture(t);
  const content = "synthetic dmg";
  writeFileSync(join(f.root, f.name + ".dmg"), content);
  Object.assign(f.manifest, {
    mode: "release-self-signed-distribution",
    signing: "local-certificate-not-notarized",
    distributionSignatureVerified: false,
    codeSignatureVerified: true,
    notarization: "not-notarized",
    localSigningIdentity: "A".repeat(40),
    localSigningRequirement:
      'identifier "com.osir.client.beta" and certificate leaf = H"' +
      "A".repeat(40) +
      '"',
    installerArtifact: {
      name: f.name + ".dmg",
      sha256: digest(content),
      bytes: Buffer.byteLength(content),
    },
  });
  const save = () =>
    writeFileSync(
      join(f.root, "build-manifest.json"),
      JSON.stringify(f.manifest),
    );
  save();
  const plan = createReleasePlan(f.options);
  assert.equal(plan.distribution_signature_verified, false);
  assert.equal(plan.updater_signature_verified, true);
  assert.match(plan.notes, /未经 Apple 公证/);
  f.manifest.localSigningRequirement = 'cdhash H"' + "A".repeat(40) + '"';
  save();
  assert.throws(() => createReleasePlan(f.options));
});

test("universal archive binds both Tauri platforms to the same verified bytes", (t) => {
  const f = fixture(t, true),
    p = createReleasePlan(f.options);
  assert.deepEqual(
    p.artifacts.map((a) => a.platform),
    ["darwin-aarch64", "darwin-aarch64", "darwin-x86_64", "darwin-x86_64"],
  );
  assert.equal(new Set(p.artifacts.map((a) => a.file_name)).size, 2);
});

test("unsigned, changed installer/updater/public config and unsupported targets fail locally", (t) => {
  for (const mutation of [
    (f) => {
      f.manifest.distributionSignatureVerified = false;
    },
    (f) => {
      f.manifest.mode = "local-unsigned";
    },
    (f) => {
      f.manifest.updaterRuntimeConfigured = false;
    },
    (f) => {
      f.manifest.sourceProvenance.verifiedAtEnd = false;
    },
    (f) => {
      f.manifest.architectures = ["x86_64"];
    },
    (f) => {
      writeFileSync(join(f.root, f.name + ".zip"), "changed");
    },
    (f) => {
      writeFileSync(join(f.root, f.name + ".app.tar.gz"), "changed");
    },
    (f) => {
      writeFileSync(
        f.configPath,
        JSON.stringify({ ...f.config, notes: "changed" }),
      );
    },
  ]) {
    const f = fixture(t);
    mutation(f);
    writeFileSync(
      join(f.root, "build-manifest.json"),
      JSON.stringify(f.manifest),
    );
    assert.throws(() => createReleasePlan(f.options));
  }
});

test("artifact symlinks are rejected without reading the target", (t) => {
  const f = fixture(t),
    link = join(f.root, "link.zip");
  symlinkSync(join(f.root, f.name + ".zip"), link);
  assert.throws(() => inspectArtifact(link), /symlinks/);
});

test("draft registration is idempotent and contains only release metadata", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options),
    calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, init });
    assert.equal(init.redirect, "error");
    if (init.method === "POST") {
      const body = JSON.parse(init.body);
      assert.equal(body.source_sha, plan.source_sha);
      assert.equal(body.notes, "Test release");
      assert.equal(
        body.artifacts.some((a) => "path" in a || "file_name" in a),
        false,
      );
      return reply(publicDraft(plan), 201, url);
    }
    assert.fail("Registration must not read artifacts before cloud upload.");
  };
  assert.equal(
    (await runReleaseAction(plan, { action: "register", token, fetchImpl }))
      .readback_verified,
    false,
  );
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls.length, 1);
});

test("draft verification performs full public byte readback after upload", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options),
    calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, init });
    if (url.includes("/admin/")) return reply(publicDraft(plan), 200, url);
    assert.equal(init.headers.authorization, undefined);
    return artifactReply(plan.artifacts.find((a) => a.url === url));
  };
  assert.equal(
    (await runReleaseAction(plan, { action: "verify", token, fetchImpl }))
      .readback_verified,
    true,
  );
  assert.equal(calls.length, 3);
});

test("bad public bytes fail verification and never publish", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options);
  let posts = 0;
  const fetchImpl = async (url, init) => {
    if (url.includes("/admin/")) {
      posts++;
      return reply(publicDraft(plan), 201, url);
    }
    return artifactReply(
      plan.artifacts.find((a) => a.url === url),
      true,
    );
  };
  await assert.rejects(
    runReleaseAction(plan, { action: "verify", token, fetchImpl }),
    /size differs|hash differs/,
  );
  assert.equal(posts, 1);
});

test("publish reads exact draft and bytes before compare-and-swap activation", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options),
    calls = [];
  let activated = false;
  const fetchImpl = async (url, init) => {
    calls.push({ url, init });
    if (url.endsWith("/releases/beta"))
      return activated
        ? reply({ release_id: plan.release_id }, 200, url)
        : reply(null, 204, url);
    if (url.endsWith("/updater/beta.json"))
      return new Response(
        JSON.stringify({
          version: plan.version,
          platforms: Object.fromEntries(
            plan.artifacts
              .filter((a) => a.kind === "updater")
              .map((a) => [
                a.platform,
                {
                  signature: a.signature,
                  url: `https://osirclaw.com/api/v1/osir-client/download/${plan.release_id}/${a.platform}/updater`,
                },
              ]),
          ),
        }),
      );
    if (url.endsWith("/publish")) {
      assert.deepEqual(JSON.parse(init.body), {
        expected_current_release_id: null,
      });
      activated = true;
      return reply({ ...publicDraft(plan), status: "published" }, 200, url);
    }
    if (url.includes("/admin/")) return reply(publicDraft(plan), 200, url);
    return artifactReply(plan.artifacts.find((a) => a.url === url));
  };
  assert.equal(
    (
      await runReleaseAction(plan, {
        action: "publish",
        token,
        expectedCurrent: "none",
        fetchImpl,
      })
    ).status,
    "published",
  );
  assert.ok(calls.at(-1).url.endsWith("/updater/beta.json"));
  assert.ok(
    calls.findIndex((c) => c.url.endsWith("/publish")) >
      calls.findIndex((c) => c.url.includes("/artifacts/")),
  );
});

test("unconfirmed public feed after publish is reported as uncertain without a second publish", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options);
  let posts = 0;
  const fetchImpl = async (url, init) => {
    if (url.endsWith("/releases/beta"))
      return posts
        ? reply({ release_id: plan.release_id }, 200, url)
        : reply(null, 204, url);
    if (url.endsWith("/publish")) {
      posts++;
      return reply({ ...publicDraft(plan), status: "published" }, 200, url);
    }
    if (url.includes("/admin/")) return reply(publicDraft(plan), 200, url);
    if (url.includes("/updater/"))
      return new Response("unavailable", { status: 503 });
    return artifactReply(plan.artifacts.find((a) => a.url === url));
  };
  await assert.rejects(
    runReleaseAction(plan, {
      action: "publish",
      token,
      expectedCurrent: "none",
      fetchImpl,
    }),
    /Publish was submitted.*not confirmed/,
  );
  assert.equal(posts, 1);
});

test("draft idempotency key does not depend on local output directory", async (t) => {
  const plans = [
      createReleasePlan(fixture(t).options),
      createReleasePlan(fixture(t).options),
    ],
    keys = [];
  for (const plan of plans) {
    await runReleaseAction(plan, {
      action: "draft",
      token,
      fetchImpl: async (url, init) => {
        if (init.method === "POST") {
          keys.push(JSON.parse(init.body).idempotency_key);
          return reply(publicDraft(plan), 201, url);
        }
        return artifactReply(plan.artifacts.find((a) => a.url === url));
      },
    });
  }
  assert.equal(keys[0], keys[1]);
});

test("pointer race or modified cloud draft cannot publish", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options);
  let publish = 0;
  for (const raced of [true, false]) {
    const fetchImpl = async (url, init) => {
      if (url.endsWith("/releases/beta"))
        return raced
          ? reply({ release_id: "beta-other" }, 200, url)
          : reply(null, 204, url);
      if (url.endsWith("/publish")) {
        publish++;
        return reply({}, 200, url);
      }
      return reply(
        { ...publicDraft(plan), source_sha: "c".repeat(64) },
        200,
        url,
      );
    };
    await assert.rejects(
      runReleaseAction(plan, {
        action: "publish",
        token,
        expectedCurrent: "none",
        fetchImpl,
      }),
      /pointer changed|draft differs/,
    );
  }
  assert.equal(publish, 0);
});

test("credentials are never included in API error text and auth requests reject redirects", async (t) => {
  const f = fixture(t),
    plan = createReleasePlan(f.options);
  const fetchImpl = async (url, init) => {
    assert.equal(init.redirect, "error");
    return new Response(JSON.stringify({ message: token }), { status: 401 });
  };
  await assert.rejects(
    runReleaseAction(plan, { action: "draft", token, fetchImpl }),
    (error) => !error.message.includes(token) && error.message.includes("401"),
  );
});

test("Windows release verifies the actual installer updater signature without claiming Authenticode", (t) => {
  const f = fixture(t);
  const name = `OSIR-Client-beta-${f.manifest.version}-x86_64-pc-windows-msvc-setup.exe`;
  writeFileSync(join(f.root, name), "test");
  writeFileSync(join(f.root, name + ".sig"), signature + "\n");
  Object.assign(f.manifest, {
    mode: "release-windows-distribution",
    target: "x86_64-pc-windows-msvc",
    architectures: ["x86_64"],
    distributionSignatureVerified: false,
    codeSignatureVerified: false,
    notarization: "not-applicable",
    signing: "windows-unsigned-updater-signed",
    peArchitectureVerified: true,
    installerArtifact: { name, sha256: digest("test"), bytes: 4 },
  });
  f.feed.platforms = {
    "windows-x86_64": {
      signature,
      url: `https://osirclaw.com/client-release/${name}`,
    },
  };
  writeFileSync(join(f.root, "updater-beta.json"), JSON.stringify(f.feed));
  const save = () =>
    writeFileSync(
      join(f.root, "build-manifest.json"),
      JSON.stringify(f.manifest),
    );
  save();
  const plan = createReleasePlan(f.options);
  assert.equal(plan.code_signature_verified, false);
  assert.equal(plan.updater_signature_verified, true);
  assert.match(plan.notes, /Authenticode/);
  assert.deepEqual(
    plan.artifacts.map((a) => [a.platform, a.kind]),
    [
      ["windows-x86_64", "updater"],
      ["windows-x86_64", "installer"],
    ],
  );
  f.manifest.peArchitectureVerified = false;
  save();
  assert.throws(() => createReleasePlan(f.options));
});
