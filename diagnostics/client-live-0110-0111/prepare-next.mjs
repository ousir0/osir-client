// Local file-only preparation. Never fetches, publishes, launches or installs.
import assert from 'node:assert/strict';
import {existsSync,readFileSync,writeFileSync} from 'node:fs';
import {isAbsolute,join} from 'node:path';
import {ROOT,COMMIT,SOURCE,BUILD_RUNS,PLATFORMS,sha,verifyLocks,validateSpec,validateManifest,validateFeed} from './contract.mjs';
import {inputs,objectUrl} from './reviewed/prepare.mjs';
verifyLocks();
const [command,contextPath,output]=process.argv.slice(2);
if(command==='inspect'){
 console.log(JSON.stringify({status:'prepared-not-dispatched-not-updater-acceptance',fromVersion:inputs.fromVersion,toVersion:inputs.toVersion,sourceCommit:COMMIT,sourceSha256:SOURCE,buildRuns:BUILD_RUNS,baselinePlatforms:inputs.baseline.map(p=>({platform:p.platform,sha256:p.sha256,size:p.size_bytes,executableSha256:p.executableSha256})),missingFinalManifests:Object.keys(PLATFORMS).filter(p=>!existsSync(join(ROOT,`final-manifests/${p}.json`))),pending:['reviewed control commit','successful exact build attempts and finalized artifact provenance','real public cutover receipt and feed bytes'],nativeExecution:'not-run'},null,2));
}else if(command==='assemble'){
 assert.ok(isAbsolute(contextPath)&&isAbsolute(output),'Absolute explicit local paths required');
 const context=JSON.parse(readFileSync(contextPath,'utf8'));
 assert.deepEqual(Object.keys(context).sort(),['harnessCommit','cutoverConfirmed','cutoverConfirmedAt','cutoverReceiptPath','publicFeedPath'].sort());
 assert.equal(context.cutoverConfirmed,true,'Coordinator must explicitly confirm real public cutover');
 for(const key of ['cutoverReceiptPath','publicFeedPath'])assert.ok(isAbsolute(context[key]));
 const raw=readFileSync(context.publicFeedPath),feed=JSON.parse(raw);
 const spec={schemaVersion:1,fromVersion:'0.1.10',toVersion:'0.1.11',sourceCommit:COMMIT,sourceSha256:SOURCE,harnessCommit:context.harnessCommit,cutover:{confirmed:true,confirmedAt:context.cutoverConfirmedAt,receiptSha256:sha(readFileSync(context.cutoverReceiptPath))},feed:{url:inputs.endpoint,sha256:sha(raw),size:raw.length},platforms:[]};
 for(const [platform,policy] of Object.entries(PLATFORMS)){
  const manifestPath=`final-manifests/${platform}.json`,bytes=readFileSync(join(ROOT,manifestPath)),m=JSON.parse(bytes);
  const name=`OSIR-Client-prod-0.1.11-${policy.target}${platform.startsWith('darwin')?'.app.tar.gz':'-setup.exe'}`;
  spec.platforms.push({platform,manifestPath,manifestSha256:sha(bytes),manifestSize:bytes.length,executableSha256:m.releaseDiagnostics?.executableSha256,buildRun:{id:BUILD_RUNS[platform].id,attempt:BUILD_RUNS[platform].attempt},updater:{objectUrl:objectUrl('0.1.11',name),feedUrl:feed.platforms?.[platform]?.url,sha256:m.updaterArtifact?.sha256,size:m.updaterArtifact?.bytes,signature:feed.platforms?.[platform]?.signature}});
 }
 validateSpec(spec);for(const p of spec.platforms)validateManifest(spec,p,readFileSync(join(ROOT,p.manifestPath)));validateFeed(spec,raw);
 assert.ok(Date.parse(spec.cutover.confirmedAt)<=Date.now(),'Future cutover refused');
 const bytes=Buffer.from(JSON.stringify(spec));writeFileSync(output,bytes,{flag:'wx',mode:0o600});
 console.log(JSON.stringify({status:'assembled-not-dispatched',path:output,size:bytes.length,sha256:sha(bytes)}));
}else throw Error('Use inspect, or assemble <absolute-context.json> <new-absolute-public-spec.json>');
