// External acceptance preparation only. Never launches, stops, installs or publishes an app.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {createWriteStream, lstatSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {isAbsolute, join, resolve, basename} from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {Readable} from 'node:stream';
import {pipeline} from 'node:stream/promises';

const directory = fileURLToPath(new URL('.', import.meta.url));
export const inputs = JSON.parse(readFileSync(join(directory, 'inputs.json')));
const folders = {'darwin-aarch64':'mac-arm64', 'darwin-x86_64':'mac-intel', 'windows-x86_64':'windows-x64'};
const targets = {'darwin-aarch64':'aarch64-apple-darwin', 'darwin-x86_64':'x86_64-apple-darwin', 'windows-x86_64':'x86_64-pc-windows-msvc'};
const architectures = {'darwin-aarch64':'arm64', 'darwin-x86_64':'x86_64', 'windows-x86_64':'x86_64'};
const sha = value => assert.match(value ?? '', /^[a-f0-9]{64}$/);
export function digest(path) {return createHash('sha256').update(readFileSync(path)).digest('hex');}
function json(path) {return JSON.parse(readFileSync(path, 'utf8'));}
function record(path) {return {sha256:digest(path), bytes:lstatSync(path).size};}
function regular(path) {
  assert.equal(typeof path, 'string'); assert.ok(isAbsolute(path), 'Absolute local artifact path required');
  const stat = lstatSync(path); assert.ok(stat.isFile() && !stat.isSymbolicLink(), 'Regular artifact required');
  return path;
}
function save(path, value) {writeFileSync(path, JSON.stringify(value,null,2)+'\n', {flag:'wx',mode:0o600});}

export function objectUrl(version, name) {
  assert.ok(['0.1.10','0.1.11'].includes(version));
  assert.match(name, /^OSIR-Client-prod-0\.1\.(?:10|11)-(?:aarch64-apple-darwin|x86_64-apple-darwin|universal-apple-darwin|x86_64-pc-windows-msvc-setup)\.(?:app\.tar\.gz|exe)$/);
  assert.ok(name.startsWith(`OSIR-Client-prod-${version}-`));
  return `https://osirvedio.cn-nb1.rains3.com/osir-client/releases/prod-${version}/${name}`;
}

export function assertTransition(spec) {
  assert.equal(spec.schemaVersion,1); assert.equal(spec.fromVersion,'0.1.10'); assert.equal(spec.toVersion,'0.1.11');
  assert.notEqual(spec.fromVersion,spec.toVersion,'Same-version installation is not an update');
  sha(spec.expectedSourceSha256); assert.match(spec.expectedGitHead ?? '',/^[a-f0-9]{40}$/);
  assert.match(spec.pubDate ?? '',/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/);
  assert.ok(Number.isFinite(Date.parse(spec.pubDate)));
  assert.equal(typeof spec.notes,'string'); assert.ok(spec.notes.length <= 8192);
  assert.deepEqual(spec.platforms.map(x=>x.platform).sort(),Object.keys(folders).sort(),'Exactly Mac ARM, Intel and Windows x64 are required');
}

export function assertCandidateManifest(manifest, entry, spec) {
  assert.ok(Object.hasOwn(targets,entry.platform));
  assert.equal(manifest.version,spec.toVersion); assert.equal(manifest.channel,'prod');
  assert.equal(manifest.identifier,'com.osir.client');
  assert.equal(manifest.sourceProvenance?.sourceSha256,spec.expectedSourceSha256);
  assert.equal(manifest.sourceProvenance?.git?.head,spec.expectedGitHead);
  const universal = entry.platform.startsWith('darwin-') && manifest.target === 'universal-apple-darwin';
  assert.equal(manifest.target,universal ? 'universal-apple-darwin' : targets[entry.platform]);
  assert.deepEqual([...manifest.architectures].sort(),universal ? ['arm64','x86_64'] : [architectures[entry.platform]]);
  const diagnostic = manifest.releaseDiagnostics;
  sha(diagnostic?.executableSha256);
  assert.equal(diagnostic.report.version,spec.toVersion);
  assert.equal(diagnostic.report.identifier,'com.osir.client');
  assert.deepEqual(diagnostic.report.updater?.endpoints,[inputs.endpoint]);
  assert.equal(diagnostic.report.updater.pubkey,inputs.pubkey);
  assert.equal(manifest.updaterEnabled,true); assert.equal(manifest.updaterRuntimeConfigured,true);
  const target = universal ? 'universal-apple-darwin' : targets[entry.platform];
  const suffix = entry.platform.startsWith('darwin-') ? `${target}.app.tar.gz` : `${target}-setup.exe`;
  const name = `OSIR-Client-prod-${spec.toVersion}-${suffix}`;
  assert.equal(basename(entry.payload),name);
  assert.equal(entry.objectUrl,objectUrl(spec.toVersion,name),'Only exact immutable object storage URLs are allowed');
  return {name, target, executableSha256:diagnostic.executableSha256};
}

async function verifier(path) {
  regular(path);
  const pinned = {sha256:inputs.signatureVerifierSha256};
  assert.ok(pinned); assert.equal(digest(path),pinned.sha256,'Read-only signature verifier source differs from reviewed 0.1.10 source');
  return import(pathToFileURL(path).href);
}

// Full public-byte readback; there is no main-site byte proxy or redirect fallback.
async function download(url, expected, destination) {
  const response = await fetch(url,{redirect:'error',credentials:'omit',signal:AbortSignal.timeout(1_200_000)});
  assert.equal(response.status,200); assert.equal(response.url,url);
  assert.equal(Number(response.headers.get('content-length')),expected.bytes);
  let bytes=0; const hash=createHash('sha256');
  async function* observe(stream) {
    for await (const chunk of stream) {
      bytes+=chunk.length; assert.ok(bytes<=expected.bytes,'Object exceeds pinned size'); hash.update(chunk); yield chunk;
    }
  }
  await pipeline(observe(Readable.fromWeb(response.body)),createWriteStream(destination,{flags:'wx',mode:0o600}));
  assert.equal(bytes,expected.bytes); assert.equal(hash.digest('hex'),expected.sha256);
  return {url,bytes,sha256:expected.sha256,readback:'complete',redirects:0};
}

export async function main(args) {
  const [mode,inputPath,outputPath,verifierPath]=args;
  assert.ok(['baseline-local','baseline-download','candidate'].includes(mode));
  assert.ok(isAbsolute(outputPath));
  const verify = await verifier(verifierPath);
  const report={schemaVersion:1,status:'preparing',scope:'artifact-preparation-only; no application or updater was executed',fromVersion:'0.1.10',toVersion:'0.1.11',artifacts:[]};
  mkdirSync(outputPath,{mode:0o700}); // Existing output is refused; do not reuse old evidence.
  try {
    if (mode.startsWith('baseline-')) {
      for (const item of inputs.baseline) {
        const path=mode==='baseline-local' ? join(inputPath,'release',folders[item.platform],item.file_name) : join(outputPath,item.file_name);
        if(mode==='baseline-download') await download(item.objectUrl,{bytes:item.size_bytes,sha256:item.sha256},path);
        regular(path);
        const result=verify.verifyUpdaterArtifact(path,item.signature,inputs.pubkey);
        assert.equal(result.bytes,item.size_bytes); assert.equal(result.sha256,item.sha256);
        report.artifacts.push({platform:item.platform,...result,objectUrl:item.objectUrl,publicReadback:mode==='baseline-download'?'complete':'not-run-local-archive-only'});
      }
    } else {
      const spec=json(regular(inputPath)); assertTransition(spec);
      const platforms={}, publicDownloads=new Map();
      for(const entry of spec.platforms) {
        regular(entry.manifest); regular(entry.payload); regular(entry.signature);
        const manifest=json(entry.manifest);
        const identity=assertCandidateManifest(manifest,entry,spec);
        const signature=readFileSync(entry.signature,'utf8').trim();
        const result=verify.verifyUpdaterArtifact(entry.payload,signature,inputs.pubkey);
        assert.equal(result.bytes,manifest.updaterArtifact?.bytes);
        assert.equal(result.sha256,manifest.updaterArtifact?.sha256);
        const prior=publicDownloads.get(entry.objectUrl);
        if(prior) {assert.equal(prior.sha256,result.sha256); assert.equal(prior.bytes,result.bytes);}
        else publicDownloads.set(entry.objectUrl,await download(entry.objectUrl,result,join(outputPath,identity.name)));
        platforms[entry.platform]={url:entry.objectUrl,signature};
        report.artifacts.push({platform:entry.platform,...identity,...result,manifest:record(entry.manifest),signatureFile:record(entry.signature),publicReadback:publicDownloads.get(entry.objectUrl)});
      }
      report.candidateSpec=record(inputPath); report.gitHead=spec.expectedGitHead; report.sourceSha256=spec.expectedSourceSha256;
      save(join(outputPath,'candidate-feed.json'),{version:spec.toVersion,notes:spec.notes,pub_date:spec.pubDate,platforms});
      report.feed=record(join(outputPath,'candidate-feed.json'));
    }
    report.status='prepared-not-executed';
  }catch(error) {report.status='preparation-failed';report.error=String(error.message);throw error;}
  finally {save(join(outputPath,'preparation.json'),report);}
  return report;
}

if(process.argv[1] && resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then(r=>console.log(JSON.stringify({status:r.status,artifacts:r.artifacts.length}))).catch(error=>{console.error(error.message);process.exitCode=1;});
}
