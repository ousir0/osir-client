// External live-feed acceptance only. Importing is inert; no app/process control.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {createWriteStream,lstatSync,mkdirSync,readFileSync,realpathSync,writeFileSync} from 'node:fs';
import {basename,dirname,join,relative,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {Readable} from 'node:stream';
import {pipeline} from 'node:stream/promises';
import {inputs,objectUrl,assertCandidateManifest} from './reviewed/prepare.mjs';
import {verifyUpdaterArtifact} from './reviewed/osir-release-updater.mjs';
export const ROOT=dirname(fileURLToPath(import.meta.url));
export const LOCKED_PATHS=[
 'prepare-next.mjs','baseline-feed.json',
 'baseline-manifests/darwin-aarch64.json','baseline-manifests/darwin-x86_64.json','baseline-manifests/windows-x86_64.json',
 '.gitattributes','README.md','NEXT-ANYWHERE-CONTRACT.md','contract.mjs','contract.test.mjs','live-macos.py','live-windows.ps1',
 'workflow.review.yml','bridge.review.yml','test_harness.py','seal.mjs','reviewed-provenance.json',
 'helpers/check-ui.mjs','helpers/click.swift','helpers/gui-preflight.swift',
 'helpers/pe-arch.py','helpers/wait-schema.py','helpers/windows-window.cs',
 'reviewed/inputs.json','reviewed/prepare.mjs','reviewed/prepare.test.mjs',
 'reviewed/disposable.py','reviewed/test_disposable.py','reviewed/osir-release-updater.mjs',
 'reviewed/osir-live-updater-check.mjs','reviewed/osir-updater-click.swift',
 'reviewed/osir-macos-window-probe.swift',
];
export const COMMIT='902ac8bc50bda2fa25ac54f8f24b12a15797ae32';
export const SOURCE='a9a7c27e85c1fcf4b1f2805e8fbe8913e43c069688ffcb6abd494a33e7ee52ae';
export const BUILD_RUNS={
 'darwin-aarch64':{id:43601475491,attempt:1,path:'.github/workflows/osir-macos-arm-release.yml'},
 'darwin-x86_64':{id:43601479837,attempt:1,path:'.github/workflows/osir-windows-release.yml'},
 'windows-x86_64':{id:43601479837,attempt:1,path:'.github/workflows/osir-windows-release.yml'},
};
export const PLATFORMS={
 'darwin-aarch64':{runner:'macos-15',target:'aarch64-apple-darwin',arch:'arm64'},
 'darwin-x86_64':{runner:'macos-15-intel',target:'x86_64-apple-darwin',arch:'x86_64'},
 'windows-x86_64':{runner:'windows-2022',target:'x86_64-pc-windows-msvc',arch:'x86_64'},
};
export const sha=data=>createHash('sha256').update(data).digest('hex');
const hex=(v,n)=>{assert.equal(typeof v,'string');assert.match(v,new RegExp(`^[a-f0-9]{${n}}$`));assert.ok(!/^(.)\1+$/.test(v),'Placeholder digest refused');};
const keys=(o,list)=>{assert.ok(o&&typeof o==='object'&&!Array.isArray(o));assert.deepEqual(Object.keys(o).sort(),list.sort(),'Missing/unknown input field');};
function size(v,max=2*1024**3){assert.ok(Number.isSafeInteger(v)&&v>0&&v<=max,'Invalid exact byte size');}
export function validateSpec(s){
 keys(s,['schemaVersion','fromVersion','toVersion','sourceCommit','sourceSha256','harnessCommit','cutover','feed','platforms']);
 assert.equal(s.schemaVersion,1);assert.equal(s.fromVersion,'0.1.10');assert.equal(s.toVersion,'0.1.11');
 assert.equal(s.sourceCommit,COMMIT);assert.equal(s.sourceSha256,SOURCE);hex(s.harnessCommit,40);
 keys(s.cutover,['confirmed','confirmedAt','receiptSha256']);assert.equal(s.cutover.confirmed,true);
 assert.match(s.cutover.confirmedAt,/^2026-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/);assert.ok(Number.isFinite(Date.parse(s.cutover.confirmedAt)));hex(s.cutover.receiptSha256,64);
 keys(s.feed,['url','sha256','size']);assert.equal(s.feed.url,inputs.endpoint);hex(s.feed.sha256,64);size(s.feed.size,128*1024);
 assert.ok(Array.isArray(s.platforms));assert.deepEqual(s.platforms.map(p=>p.platform).sort(),Object.keys(PLATFORMS).sort(),'All three native platforms required');
 for(const p of s.platforms){
  keys(p,['platform','manifestPath','manifestSha256','manifestSize','executableSha256','buildRun','updater']);
  keys(p.buildRun,['id','attempt']);assert.equal(p.buildRun.id,BUILD_RUNS[p.platform].id);assert.equal(p.buildRun.attempt,BUILD_RUNS[p.platform].attempt);
  assert.equal(p.manifestPath,`final-manifests/${p.platform}.json`);hex(p.manifestSha256,64);size(p.manifestSize,1024*1024);hex(p.executableSha256,64);
  keys(p.updater,['objectUrl','feedUrl','sha256','size','signature']);const u=p.updater;
  const target=PLATFORMS[p.platform].target;const name=`OSIR-Client-prod-0.1.11-${target}${p.platform.startsWith('darwin')?'.app.tar.gz':'-setup.exe'}`;
  assert.equal(u.objectUrl,objectUrl('0.1.11',name));
  assert.ok([u.objectUrl,`https://osirclaw.com/api/v1/osir-client/artifacts/prod-0.1.11/${name}`,`https://osirclaw.com/api/v1/osir-client/download/prod-0.1.11/${p.platform}/updater`].includes(u.feedUrl),'Exact approved feed artifact path required');
  hex(u.sha256,64);size(u.size);assert.equal(typeof u.signature,'string');assert.match(u.signature,/^[A-Za-z0-9+/]+={0,2}$/);assert.ok(u.signature.length>=128&&u.signature.length<=16384);assert.equal(Buffer.from(u.signature,'base64').toString('base64'),u.signature);
 }
 return s;
}
// Public run metadata is fetched without credentials; no dispatch or mutation.
export function validateBuildRun(platform,run){
 const expected=BUILD_RUNS[platform];assert.ok(expected);
 assert.equal(run.id,expected.id);assert.equal(run.run_attempt,expected.attempt);
 assert.equal(run.repository?.full_name,'ousir0/osir-client');
 assert.equal(run.head_sha,COMMIT);assert.equal(run.head_branch,'codex/release-build');
 assert.equal(run.path,expected.path);assert.equal(run.event,'workflow_dispatch');
 assert.equal(run.status,'completed','Bound build is not complete');assert.equal(run.conclusion,'success','Bound build must succeed');
 return {id:run.id,attempt:run.run_attempt,headSha:run.head_sha,path:run.path,status:run.status,conclusion:run.conclusion};
}
export async function verifyBuildRuns(s,readRun=async id=>{
 const response=await fetch(`https://api.github.com/repos/ousir0/osir-client/actions/runs/${id}`,{redirect:'error',credentials:'omit',headers:{Accept:'application/vnd.github+json'},signal:AbortSignal.timeout(30000)});
 assert.equal(response.status,200,'Cannot read exact public build run');return response.json();
}){
 validateSpec(s);const seen=new Map(),report={};
 for(const p of s.platforms){if(!seen.has(p.buildRun.id))seen.set(p.buildRun.id,await readRun(p.buildRun.id));report[p.platform]=validateBuildRun(p.platform,seen.get(p.buildRun.id));}
 return report;
}
export function verifyBaselineInputs(root=ROOT){
 const baseline=JSON.parse(readFileSync(join(root,'reviewed/inputs.json'),'utf8'));
 assert.equal(baseline.fromVersion,'0.1.10');assert.equal(baseline.toVersion,'0.1.11');
 assert.equal(baseline.baselineGitHead,'ec2b05bab6224334e91922cd70c71faf8e9aee89');
 assert.equal(baseline.baselineSourceSha256,'ca4c28e21069dda1cc948e55e5ef8e1438161d96a42a21c0971a8f8848a04e73');
 const feed=JSON.parse(readFileSync(join(root,'baseline-feed.json'),'utf8'));assert.equal(feed.version,'0.1.10');
 assert.deepEqual(baseline.baseline.map(p=>p.platform).sort(),Object.keys(PLATFORMS).sort());
 for(const p of baseline.baseline){
  const raw=readFileSync(join(root,`baseline-manifests/${p.platform}.json`)),m=JSON.parse(raw);
  assert.equal(sha(raw),p.manifestSha256);assert.equal(m.version,'0.1.10');assert.equal(m.channel,'prod');
  assert.equal(m.sourceProvenance?.git?.head,baseline.baselineGitHead);assert.equal(m.sourceProvenance?.sourceSha256,baseline.baselineSourceSha256);
  assert.equal(m.releaseDiagnostics?.executableSha256,p.executableSha256);assert.equal(m.updaterArtifact?.sha256,p.sha256);assert.equal(m.updaterArtifact?.bytes,p.size_bytes);assert.equal(m.updaterArtifact?.signatureVerified,true);
  assert.equal(feed.platforms[p.platform].signature,p.signature);assert.equal(feed.platforms[p.platform].url,p.url);
  assert.equal(p.objectUrl,objectUrl('0.1.10',p.file_name));
  assert.equal(m.releaseDiagnostics.report.updater.pubkey,baseline.pubkey);assert.deepEqual(m.releaseDiagnostics.report.updater.endpoints,[baseline.endpoint]);
 }
 assert.equal(sha(readFileSync(join(root,'reviewed/osir-release-updater.mjs'))),baseline.signatureVerifierSha256);
 return true;
}
export function validateManifest(s,p,bytes){
 assert.equal(bytes.length,p.manifestSize);assert.equal(sha(bytes),p.manifestSha256,'Final manifest bytes changed');
 const m=JSON.parse(bytes.toString('utf8'));
 assertCandidateManifest(m,{platform:p.platform,payload:'/'+basename(p.updater.objectUrl),objectUrl:p.updater.objectUrl},{toVersion:s.toVersion,expectedGitHead:COMMIT,expectedSourceSha256:SOURCE});
 assert.equal(m.target,PLATFORMS[p.platform].target);assert.deepEqual(m.architectures,[PLATFORMS[p.platform].arch]);
 assert.equal(m.releaseDiagnostics.executableSha256,p.executableSha256);
 assert.equal(m.updaterArtifact?.sha256,p.updater.sha256);assert.equal(m.updaterArtifact?.bytes,p.updater.size);assert.equal(m.updaterArtifact?.signatureVerified,true);
 assert.equal(m.sourceProvenance?.verifiedAtEnd,true);assert.ok(m.mode?.startsWith('release-')&&!m.mode.includes('pending'));
 return m;
}
export function validateFeed(s,bytes){
 assert.equal(bytes.length,s.feed.size);assert.equal(sha(bytes),s.feed.sha256,'Live feed changed after reviewed cutover');
 const f=JSON.parse(bytes.toString('utf8'));assert.equal(f.version,'0.1.11');
 assert.deepEqual(Object.keys(f.platforms).sort(),Object.keys(PLATFORMS).sort());
 for(const p of s.platforms){assert.equal(f.platforms[p.platform].url,p.updater.feedUrl);assert.equal(f.platforms[p.platform].signature,p.updater.signature);}
 return f;
}
export function validateObservation(s,p,o){
 assert.equal(o.platform,p.platform);assert.equal(o.fromVersion,'0.1.10');assert.equal(o.toVersion,'0.1.11');
 assert.equal(o.sourceCommit,COMMIT);assert.equal(o.sourceSha256,SOURCE);assert.equal(o.manifestSha256,p.manifestSha256);
 assert.ok(Number.isSafeInteger(o.oldPid)&&o.oldPid>0);assert.ok(Number.isSafeInteger(o.newPid)&&o.newPid>0&&o.newPid!==o.oldPid);
 assert.equal(o.oldExecutableSha256,inputs.baseline.find(x=>x.platform===p.platform).executableSha256);assert.equal(o.newExecutableSha256,p.executableSha256);assert.notEqual(o.oldExecutableSha256,o.newExecutableSha256);
 assert.equal(o.oldPath,o.newPath);assert.equal(o.architecture,PLATFORMS[p.platform].arch);
 for(const flag of ['realUiClick','oldPidExited','automaticRestart','nativeWindow','syntheticDataPreserved','nativeVersionVerified','signatureVerified'])assert.equal(o[flag],true,flag);
 assert.equal(o.harnessLaunchedAfterClick,false);assert.equal(o.accountAuthorizationCovered,false);
 assert.equal(o.oldSchema,26);assert.equal(o.newSchema,26);
 assert.equal(o.events.length,8,'Extra/missing runtime transition events');
 assert.equal(typeof o.oldPath,'string');assert.ok(o.oldPath.length>1);
 assert.ok(p.platform.startsWith('windows')?/^[A-Za-z]:[\\/]/.test(o.oldPath):o.oldPath.startsWith('/'),'Absolute installed path required');
 let last=0;for(const phase of ['old-ready','fixture-seeded','old-ready-with-fixture','update-clicked','old-exited','new-observed','new-window','data-retained']){
  const events=o.events.filter(e=>e.phase===phase);assert.equal(events.length,1,`Missing/repeated ${phase}`);const at=Date.parse(events[0].at);assert.ok(Number.isFinite(at)&&at>=last,'Invalid observation ordering');last=at;
 }
 return true;
}
export function verifyLocks(root=ROOT){
 const locks=JSON.parse(readFileSync(join(root,'helpers-lock.json'),'utf8'));
 assert.deepEqual(Object.keys(locks).sort(),[...LOCKED_PATHS].sort(),'Complete reviewed helper lock required');
 for(const [path,expected] of Object.entries(locks)){hex(expected,64);const file=join(root,path);assert.ok(lstatSync(file).isFile()&&!lstatSync(file).isSymbolicLink());assert.equal(sha(readFileSync(file)),expected,`Harness changed: ${path}`);}
 verifyBaselineInputs(root);
}
export function loadSpec(path,checkFiles=true){
 const s=validateSpec(JSON.parse(readFileSync(path,'utf8')));
 if(checkFiles)for(const p of s.platforms){const file=join(ROOT,p.manifestPath);assert.ok(lstatSync(file).isFile()&&!lstatSync(file).isSymbolicLink());assert.equal(relative(ROOT,realpathSync(file)).startsWith('..'),false);validateManifest(s,p,readFileSync(file));}
 return s;
}
export function runner(workspace,s){
 assert.equal(process.env.GITHUB_ACTIONS,'true');assert.equal(process.env.CI,'true');assert.equal(process.env.RUNNER_ENVIRONMENT,'github-hosted');
 assert.equal(process.env.GITHUB_REPOSITORY,'ousir0/osir-client');assert.equal(process.env.GITHUB_SHA,s.harnessCommit,'Dispatch must pin reviewed harness commit');assert.equal(process.env.CUTOVER_CONFIRMED,'true');
 assert.ok(Date.parse(s.cutover.confirmedAt)<=Date.now(),'Future cutover refused');
 const temp=realpathSync(process.env.RUNNER_TEMP);const root=realpathSync(workspace);
 assert.ok(relative(temp,root)&&!relative(temp,root).startsWith('..'));assert.ok(basename(root).startsWith('osir-updater-0110-to-0111-'));
 const marker=JSON.parse(readFileSync(join(root,'osir-updater-0110-to-0111-owned.json'),'utf8'));assert.deepEqual(marker,{runId:process.env.GITHUB_RUN_ID,purpose:'0110-to-0111-synthetic-only'});
 return root;
}
// Exact direct-object download. Live metadata never redirects; package readback
// never uses a main-site proxy. No TLS overrides, hosts changes or custom CA.
export async function fetchExact(url,{size:expected,sha256},destination){
 const response=await fetch(url,{redirect:'error',credentials:'omit',headers:{'Accept-Encoding':'identity'},signal:AbortSignal.timeout(expected<=128*1024?30*1000:20*60*1000)});
 assert.equal(response.status,200);assert.equal(response.url,url);assert.ok([null,'identity'].includes(response.headers.get('content-encoding')));
 if(response.headers.has('content-length'))assert.equal(Number(response.headers.get('content-length')),expected);
 let count=0;const hash=createHash('sha256');
 async function* bounded(){for await(const chunk of Readable.fromWeb(response.body)){count+=chunk.length;assert.ok(count<=expected,'Response exceeds declared size');hash.update(chunk);yield chunk;}}
 await pipeline(bounded(),createWriteStream(destination,{flags:'wx',mode:0o600}));assert.equal(count,expected);assert.equal(hash.digest('hex'),sha256);
 return {url,size:count,sha256,complete:true,redirects:0};
}
function save(path,data){writeFileSync(path,JSON.stringify(data,null,2)+'\n',{flag:'wx',mode:0o600});}
async function main([command,specPath,workspace,platform]){
 verifyLocks();const s=loadSpec(specPath);
 if(command==='validate'){console.log(JSON.stringify({status:'strict-spec-valid-not-executed',sourceCommit:COMMIT,sourceSha256:SOURCE}));return;}
 if(command==='verify-builds'){console.log(JSON.stringify({status:'build-metadata-only-not-updater-acceptance',runs:await verifyBuildRuns(s)}));return;}
 const root=runner(workspace,s),p=s.platforms.find(x=>x.platform===platform);assert.ok(p);
 const evidence=join(root,'evidence');
 if(command==='prepare'){
  const buildRuns=await verifyBuildRuns(s);
  const path=join(evidence,'live-feed-before.json');await fetchExact(s.feed.url,s.feed,path);validateFeed(s,readFileSync(path));
  const old=inputs.baseline.find(x=>x.platform===platform), oldPath=join(root,'install',old.file_name);
  const oldRead=await fetchExact(old.objectUrl,{size:old.size_bytes,sha256:old.sha256},oldPath);
  const oldSig=verifyUpdaterArtifact(oldPath,old.signature,inputs.pubkey);assert.equal(oldSig.sha256,old.sha256);assert.equal(oldSig.bytes,old.size_bytes);
  const nextPath=join(root,'install',basename(p.updater.objectUrl));
  const nextRead=await fetchExact(p.updater.objectUrl,p.updater,nextPath);const nextSig=verifyUpdaterArtifact(nextPath,p.updater.signature,inputs.pubkey);assert.equal(nextSig.sha256,p.updater.sha256);
  save(join(evidence,'artifact-preparation.json'),{status:'artifact-readback-only',specSha256:sha(readFileSync(specPath)),manifestSha256:p.manifestSha256,harnessCommit:s.harnessCommit,sourceCommit:COMMIT,sourceSha256:SOURCE,buildRuns,old:oldRead,new:nextRead,signatureVerified:true});
  save(join(root,'runtime-input.json'),{...p,old,baseline:oldPath,expectedVersion:'0.1.11',sourceCommit:COMMIT,sourceSha256:SOURCE});
 }else if(command==='recheck-feed'){
  const path=join(evidence,'live-feed-preclick.json');await fetchExact(s.feed.url,s.feed,path);validateFeed(s,readFileSync(path));
 }else if(command==='finalize'){
  const o=JSON.parse(readFileSync(join(evidence,'observation.json'),'utf8'));validateObservation(s,p,o);
  const expectedPath=platform.startsWith('darwin')?join(root,'install','OSIR Client.app','Contents','MacOS','osir_client'):join(root,'install','应用目录','osir_client.exe');
  assert.equal(realpathSync(o.newPath),realpathSync(expectedPath),'Actual installed path differs');assert.equal(sha(readFileSync(expectedPath)),p.executableSha256,'Installed bytes changed before aggregation');
  const click=JSON.parse(readFileSync(join(evidence,'updater-click.json'),'utf8'));assert.equal(click.realUiButton,'更新并重启');assert.equal(click.offeredVersion??click.offered,'0.1.11');
  const window=JSON.parse(readFileSync(join(evidence,'updated-window.json'),'utf8'));assert.equal(window.pid,o.newPid);
  if(platform.startsWith('windows')){
   const native=JSON.parse(readFileSync(join(evidence,'updater-verify.json'),'utf8'));assert.equal(native.version,'0.1.11');assert.equal(native.realRendererAndIpc,true);
   assert.equal(window.path,o.newPath);assert.equal(window.title,'OSIR Client');assert.equal(window.visible,true);assert.ok(window.width>=900&&window.height>=600);assert.equal(window.executableSha256,p.executableSha256);
   for(const pid of [o.oldPid,o.newPid]){const owner=JSON.parse(readFileSync(join(evidence,`debug-owner-${pid}.json`),'utf8'));assert.equal(owner.nativePid,pid);assert.equal(owner.loopback,true);assert.equal(owner.descendantOfOwnedNativePid,true);}
  }else{
   assert.equal(click.pid,o.oldPid);assert.equal(window.bundleIdentifier,'com.osir.client');assert.equal(window.bundlePath,dirname(dirname(dirname(o.newPath))));assert.deepEqual(window.otherInstancePids,[]);assert.ok(window.windows.some(w=>w.pid===o.newPid&&w.onScreen===true&&w.width>=900&&w.height>=600));
  }
  const retention=JSON.parse(readFileSync(join(evidence,'data-retention.json'),'utf8'));assert.equal(retention.status,'data-retention-check-only');assert.equal(retention.before.schemaVersion,26);assert.equal(retention.after.schemaVersion,26);assert.deepEqual(retention.before.state,retention.after.state);
  save(join(evidence,'result.json'),{...o,status:'passed',scope:'real-shipped-0.1.10-live-feed-updater-to-0.1.11',specSha256:sha(readFileSync(specPath)),harnessCommit:s.harnessCommit,runId:process.env.GITHUB_RUN_ID,runAttempt:process.env.GITHUB_RUN_ATTEMPT});
 }else throw Error('Unknown operation');
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))main(process.argv.slice(2)).catch(e=>{console.error(e.message);process.exitCode=1;});
