// These are synthetic contract tests, never native-updater acceptance.
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,readFileSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import {createServer} from 'node:http';
import test from 'node:test';
import {ROOT,LOCKED_PATHS,verifyLocks,COMMIT,SOURCE,PLATFORMS,validateSpec,validateManifest,validateFeed,validateObservation,fetchExact,sha} from './contract.mjs';
import {inputs,objectUrl} from './reviewed/prepare.mjs';
const H=sha('synthetic fixture hash, not real release metadata');
const S=sha('synthetic harness commit').slice(0,40);
function fixture(){
 const s={schemaVersion:1,fromVersion:'0.1.9',toVersion:'0.1.10',sourceCommit:COMMIT,sourceSha256:SOURCE,harnessCommit:S,cutover:{confirmed:true,confirmedAt:'2026-11-14T01:00:00Z',receiptSha256:H},feed:{url:inputs.endpoint,sha256:H,size:1},platforms:[]};
 const manifests={};
 for(const [platform,policy] of Object.entries(PLATFORMS)){
  const file=`OSIR-Client-prod-0.1.10-${policy.target}${platform.startsWith('darwin')?'.app.tar.gz':'-setup.exe'}`;
  const p={platform,manifestPath:`final-manifests/${platform}.json`,manifestSha256:H,manifestSize:1,executableSha256:H,updater:{objectUrl:objectUrl('0.1.10',file),feedUrl:objectUrl('0.1.10',file),size:1024,sha256:H,signature:inputs.baseline[0].signature}};
  const m={version:'0.1.10',channel:'prod',identifier:'com.osir.client',target:policy.target,architectures:[policy.arch],sourceProvenance:{sourceSha256:SOURCE,git:{head:COMMIT},verifiedAtEnd:true},releaseDiagnostics:{executableSha256:H,report:{version:'0.1.10',identifier:'com.osir.client',updater:{endpoints:[inputs.endpoint],pubkey:inputs.pubkey}}},updaterEnabled:true,updaterRuntimeConfigured:true,mode:'release-fixture-only',updaterArtifact:{sha256:H,bytes:1024,signatureVerified:true}};
  const bytes=Buffer.from(JSON.stringify(m));p.manifestSha256=sha(bytes);p.manifestSize=bytes.length;manifests[platform]=bytes;s.platforms.push(p);
 }
 const feed=Buffer.from(JSON.stringify({version:'0.1.10',platforms:Object.fromEntries(s.platforms.map(p=>[p.platform,{url:p.updater.feedUrl,signature:p.updater.signature}]))}));
 s.feed.sha256=sha(feed);s.feed.size=feed.length;return {s,manifests,feed};
}
function observation(p){return {platform:p.platform,fromVersion:'0.1.9',toVersion:'0.1.10',sourceCommit:COMMIT,sourceSha256:SOURCE,manifestSha256:p.manifestSha256,oldPid:100,newPid:200,oldExecutableSha256:inputs.baseline.find(b=>b.platform===p.platform).executableSha256,newExecutableSha256:p.executableSha256,oldPath:'/synthetic/owned/app',newPath:'/synthetic/owned/app',architecture:PLATFORMS[p.platform].arch,realUiClick:true,oldPidExited:true,automaticRestart:true,nativeWindow:true,syntheticDataPreserved:true,nativeVersionVerified:true,signatureVerified:true,harnessLaunchedAfterClick:false,accountAuthorizationCovered:false,oldSchema:25,newSchema:26,events:['old-ready','fixture-seeded','old-ready-with-fixture','update-clicked','old-exited','new-observed','new-window','data-retained'].map((phase,i)=>({phase,at:new Date(1794614400000+i*1000).toISOString()}))};}
test('complete r2 identity and exact three platforms required; missing/null placeholders refused',()=>{
 const {s}=fixture();validateSpec(s);
 for(const key of Object.keys(s)){const x=structuredClone(s);delete x[key];assert.throws(()=>validateSpec(x),key);}
 for(const change of [{sourceCommit:'a'.repeat(40)},{sourceSha256:'a'.repeat(64)},{harnessCommit:null},{fromVersion:'0.1.10'},{toVersion:'0.1.9'},{extra:'arbitrary endpoint'}])assert.throws(()=>validateSpec({...s,...change}));
 for(const key of Object.keys(s.platforms[0])){const x=structuredClone(s);delete x.platforms[0][key];assert.throws(()=>validateSpec(x),key);}
});
test('cutover, feed and immutable artifact paths cannot redirect to arbitrary sources',()=>{
 const {s}=fixture();let x=structuredClone(s);x.cutover.confirmed=false;assert.throws(()=>validateSpec(x));
 for(const url of ['http://osirclaw.com/api/v1/osir-client/updater/prod.json',inputs.endpoint+'?x=1','https://example.com/feed']){x=structuredClone(s);x.feed.url=url;assert.throws(()=>validateSpec(x));}
 for(const key of ['feedUrl','objectUrl'])for(const url of ['https://example.com/a.exe',s.platforms[0].updater.objectUrl+'?token=x','https://osirclaw.com/client-release/x.exe']){x=structuredClone(s);x.platforms[0].updater[key]=url;assert.throws(()=>validateSpec(x));}
 x=structuredClone(s);x.platforms[0].manifestPath='../data.json';assert.throws(()=>validateSpec(x));
});
test('each native platform accepts only its three exact feed URL forms with unchanged feed binding',()=>{
 const {s}=fixture();
 for(const [index,p] of s.platforms.entries()){
  const name=p.updater.objectUrl.split('/').at(-1);
  for(const url of [p.updater.objectUrl,`https://osirclaw.com/api/v1/osir-client/artifacts/prod-0.1.10/${name}`,`https://osirclaw.com/api/v1/osir-client/download/prod-0.1.10/${p.platform}/updater`]){
   const x=structuredClone(s);x.platforms[index].updater.feedUrl=url;
   const feed=Buffer.from(JSON.stringify({version:'0.1.10',platforms:Object.fromEntries(x.platforms.map(p=>[p.platform,{url:p.updater.feedUrl,signature:p.updater.signature}]))}));
   x.feed={...x.feed,size:feed.length,sha256:sha(feed)};validateSpec(x);validateFeed(x,feed);
  }
 }
});
test('production feed routes reject other releases, platforms, kinds, authorities and URL aliases',()=>{
 const {s}=fixture();
 for(const [index,p] of s.platforms.entries()){
  const url=`https://osirclaw.com/api/v1/osir-client/download/prod-0.1.10/${p.platform}/updater`;
  const other=s.platforms[(index+1)%s.platforms.length].platform;
  const invalid=[
   url.replace('prod-0.1.10','prod-0.1.9'),url.replace('prod-0.1.10','prod-0.1.11'),url.replace('prod-0.1.10','beta-0.1.10'),
   url.replace(p.platform,other),url.replace(p.platform,'linux-x86_64'),url.replace(p.platform,p.platform.toUpperCase()),
   url.replace('/updater','/installer'),url.replace('/updater','/UPDATER'),url.replace('/updater','/updater.exe'),
   url+'?x=1',url+'?',url+'#fragment',url+'#',url+'/',url+'/extra',
   url.replace('https:','http:'),url.replace('https:','HTTPS:'),url.replace('osirclaw.com','OSIRCLAW.COM'),
   url.replace('osirclaw.com','osirclaw.com.evil.example'),url.replace('osirclaw.com','evil.example'),
   url.replace('osirclaw.com','osirclaw.com:443'),url.replace('osirclaw.com','user@osirclaw.com'),url.replace('osirclaw.com','osirclaw.com@evil.example'),
   url.replace('/download/','//download/'),url.replace('/download/','/artifacts/'),url.replace('/download/','/download/./'),
   url.replace(p.platform,`other/../${p.platform}`),url.replace(p.platform,p.platform.replace('-','%2D')),url.replace('/updater','/%75pdater'),
   url.replace('/updater','\\updater'),url+'\n',
  ];
  for(const bad of invalid){
   const x=structuredClone(s);x.platforms[index].updater.feedUrl=bad;
   assert.throws(()=>validateSpec(x),`${p.platform}: ${JSON.stringify(bad)}`);
  }
  const x=structuredClone(s);x.platforms[index].updater.feedUrl=url;x.platforms[index].updater.objectUrl=url;
  assert.throws(()=>validateSpec(x),'Production feed route cannot replace immutable object URL');
 }
});
test('production feed route still binds the actual public feed URL, signature and exact bytes',()=>{
 const {s}=fixture();
 for(const p of s.platforms)p.updater.feedUrl=`https://osirclaw.com/api/v1/osir-client/download/prod-0.1.10/${p.platform}/updater`;
 const f={version:'0.1.10',platforms:Object.fromEntries(s.platforms.map(p=>[p.platform,{url:p.updater.feedUrl,signature:p.updater.signature}]))};
 const raw=Buffer.from(JSON.stringify(f));s.feed={...s.feed,size:raw.length,sha256:sha(raw)};validateSpec(s);validateFeed(s,raw);
 assert.throws(()=>validateFeed(s,Buffer.concat([raw,Buffer.from('\n')])));
 for(const [index,p] of s.platforms.entries()){
  for(const change of [entry=>entry.url=s.platforms[(index+1)%s.platforms.length].updater.feedUrl,entry=>entry.url=p.updater.objectUrl,entry=>entry.signature=inputs.baseline[1].signature]){
   const changed=structuredClone(f);change(changed.platforms[p.platform]);const bytes=Buffer.from(JSON.stringify(changed));
   assert.throws(()=>validateFeed({...s,feed:{...s.feed,size:bytes.length,sha256:sha(bytes)}},bytes));
  }
 }
});
test('final manifest raw bytes, signed artifact, native architecture and source provenance all bound',()=>{
 const {s,manifests}=fixture();for(const p of s.platforms)validateManifest(s,p,manifests[p.platform]);
 const p=s.platforms[0],original=JSON.parse(manifests[p.platform]);
 for(const change of [m=>m.sourceProvenance.git.head='c'.repeat(40),m=>m.architectures=['x86_64'],m=>m.releaseDiagnostics.executableSha256=sha('wrong'),m=>m.updaterArtifact.signatureVerified=false,m=>m.sourceProvenance.verifiedAtEnd=false]){
  const m=structuredClone(original);change(m);const b=Buffer.from(JSON.stringify(m));assert.throws(()=>validateManifest(s,{...p,manifestSha256:sha(b),manifestSize:b.length},b));
 }
 assert.throws(()=>validateManifest(s,p,Buffer.concat([manifests[p.platform],Buffer.from('\n')])));
});
test('live feed exact bytes and each URL/signature/version must match reviewed public spec',()=>{
 const {s,feed}=fixture();validateFeed(s,feed);assert.throws(()=>validateFeed(s,Buffer.concat([feed,Buffer.from('\n')])));
 for(const change of [f=>f.version='0.1.9',f=>f.platforms['darwin-aarch64'].signature=inputs.baseline[1].signature,f=>f.platforms['darwin-aarch64'].url='https://evil.example/x']){
  const f=JSON.parse(feed);change(f);const b=Buffer.from(JSON.stringify(f));assert.throws(()=>validateFeed({...s,feed:{...s.feed,size:b.length,sha256:sha(b)}},b));
 }
});
test('same-version install, manual restart, absent click/window/PID and lost data cannot pass',()=>{
 const {s}=fixture();const p=s.platforms[0],o=observation(p);validateObservation(s,p,o);
 for(const flag of ['realUiClick','oldPidExited','automaticRestart','nativeWindow','syntheticDataPreserved','nativeVersionVerified','signatureVerified'])assert.throws(()=>validateObservation(s,p,{...o,[flag]:false}),flag);
 for(const change of [{harnessLaunchedAfterClick:true},{newPid:o.oldPid},{newPath:'/another/app'},{fromVersion:'0.1.10'},{newSchema:25},{oldSchema:26},{newExecutableSha256:o.oldExecutableSha256},{architecture:'x86_64'}])assert.throws(()=>validateObservation(s,p,{...o,...change}));
});
test('missing, repeated and out-of-order transition observations cannot be aggregated as passed',()=>{
 const {s}=fixture(),p=s.platforms[0],o=observation(p);
 assert.throws(()=>validateObservation(s,p,{...o,events:o.events.slice(1)}));
 assert.throws(()=>validateObservation(s,p,{...o,events:[...o.events,o.events[0]]}));
 const events=structuredClone(o.events);events[4].at=events[0].at;assert.throws(()=>validateObservation(s,p,{...o,events}));
});
test('Windows observations require real native absolute paths, not POSIX/relative fixture paths',()=>{
 const {s}=fixture(),p=s.platforms.find(p=>p.platform==='windows-x86_64'),o={...observation(p),oldPath:'C:\\runner\\应用目录\\osir_client.exe',newPath:'C:\\runner\\应用目录\\osir_client.exe'};
 validateObservation(s,p,o);
 for(const path of ['/fixture/app','fixture/app','C:relative.exe'])assert.throws(()=>validateObservation(s,p,{...o,oldPath:path,newPath:path}));
});
test('empty/incomplete helper locks and a changed helper fail closed',()=>{
 const root=mkdtempSync(join(tmpdir(),'osir-live-lock-unit-'));
 try{
  const locks={};for(const path of LOCKED_PATHS){const file=join(root,path);mkdirSync(dirname(file),{recursive:true});const bytes=readFileSync(join(ROOT,path));writeFileSync(file,bytes);locks[path]=sha(bytes);}
  const lock=join(root,'helpers-lock.json');writeFileSync(lock,JSON.stringify(locks));verifyLocks(root);
  writeFileSync(lock,'{}');assert.throws(()=>verifyLocks(root));
  writeFileSync(lock,JSON.stringify(locks));writeFileSync(join(root,'helpers/check-ui.mjs'),'modified');assert.throws(()=>verifyLocks(root));
 }finally{rmSync(root,{recursive:true,force:true});}
});
test('direct readback rejects redirects, wrong size and corrupted bytes',async()=>{
 const temp=mkdtempSync(join(tmpdir(),'osir-live-harness-unit-'));
 const server=createServer((req,res)=>{if(req.url==='/redirect'){res.writeHead(302,{location:'/ok'});res.end();}else if(req.url==='/chunked'){res.writeHead(200);res.write('da');res.end('ta');}else{res.writeHead(200,{'content-length':4});res.end('data');}});
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const base=`http://127.0.0.1:${server.address().port}`;
 try{
  await fetchExact(base+'/ok',{size:4,sha256:sha('data')},join(temp,'ok'));
  assert.equal(readFileSync(join(temp,'ok'),'utf8'),'data');
  await fetchExact(base+'/chunked',{size:4,sha256:sha('data')},join(temp,'chunked'));
  await assert.rejects(fetchExact(base+'/chunked',{size:3,sha256:sha('data')},join(temp,'overflow')));
  await assert.rejects(fetchExact(base+'/redirect',{size:4,sha256:sha('data')},join(temp,'redirect')));
  await assert.rejects(fetchExact(base+'/ok',{size:5,sha256:sha('data')},join(temp,'size')));
  await assert.rejects(fetchExact(base+'/ok',{size:4,sha256:sha('evil')},join(temp,'hash')));
 }finally{await new Promise(r=>server.close(r));rmSync(temp,{recursive:true,force:true});}
});
