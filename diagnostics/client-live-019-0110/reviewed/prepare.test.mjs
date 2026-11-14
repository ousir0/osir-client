import assert from 'node:assert/strict';
import test from 'node:test';
import {assertTransition, assertCandidateManifest, inputs, objectUrl} from './prepare.mjs';

function spec() {return {schemaVersion:1,fromVersion:'0.1.9',toVersion:'0.1.10',expectedSourceSha256:'a'.repeat(64),expectedGitHead:'b'.repeat(40),pubDate:'2026-11-14T00:00:00Z',notes:'fixture',platforms:['darwin-aarch64','darwin-x86_64','windows-x86_64'].map(platform=>({platform}))};}
function candidate(platform='darwin-aarch64',universal=false) {
 const s=spec(),target=universal?'universal-apple-darwin':platform==='darwin-aarch64'?'aarch64-apple-darwin':'x86_64-apple-darwin';
 const name=`OSIR-Client-prod-0.1.10-${target}.app.tar.gz`;
 const entry={platform,payload:'/fixture/'+name,objectUrl:objectUrl('0.1.10',name)};
 const manifest={version:'0.1.10',channel:'prod',identifier:'com.osir.client',target,architectures:universal?['arm64','x86_64']:[platform==='darwin-aarch64'?'arm64':'x86_64'],sourceProvenance:{sourceSha256:s.expectedSourceSha256,git:{head:s.expectedGitHead}},releaseDiagnostics:{executableSha256:'c'.repeat(64),report:{version:'0.1.10',identifier:'com.osir.client',updater:{endpoints:[inputs.endpoint],pubkey:inputs.pubkey}}},updaterEnabled:true,updaterRuntimeConfigured:true};
 return {s,entry,manifest};
}
test('only exact cross-version transition with all three platforms is accepted',()=>{
 assertTransition(spec());
 for(const update of [{fromVersion:'0.1.10'},{toVersion:'0.1.9'},{expectedSourceSha256:null},{expectedGitHead:null},{platforms:[{platform:'windows-aarch64'}]}]) assert.throws(()=>assertTransition({...spec(),...update}));
});
test('ARM and Intel must match the actual architecture and frozen identity',()=>{
 for(const platform of ['darwin-aarch64','darwin-x86_64']) {const {s,entry,manifest}=candidate(platform);assertCandidateManifest(manifest,entry,s);manifest.architectures=platform==='darwin-aarch64'?['x86_64']:['arm64'];assert.throws(()=>assertCandidateManifest(manifest,entry,s));}
 const {s,entry,manifest}=candidate();manifest.sourceProvenance.git.head='d'.repeat(40);assert.throws(()=>assertCandidateManifest(manifest,entry,s));
});
test('Universal requires both slices; a single slice cannot be relabelled Universal',()=>{
 const {s,entry,manifest}=candidate('darwin-aarch64',true);assertCandidateManifest(manifest,entry,s);manifest.architectures=['arm64'];assert.throws(()=>assertCandidateManifest(manifest,entry,s));
});
test('metadata cannot change the embedded public key/endpoint or use the main server for payload bytes',()=>{
 const {s,entry,manifest}=candidate();
 assert.throws(()=>assertCandidateManifest(manifest,{...entry,objectUrl:'https://osirclaw.com/client-release/'+entry.payload.split('/').at(-1)},s));
 assert.throws(()=>assertCandidateManifest(manifest,{...entry,objectUrl:entry.objectUrl+'?token=x'},s));
 manifest.releaseDiagnostics.report.updater.pubkey='another-key';assert.throws(()=>assertCandidateManifest(manifest,entry,s));
});
test('immutable names reject traversal, credentials and version mismatch',()=>{
 for(const name of ['../x.exe','https://user:secret@example.com/x','OSIR-Client-prod-0.1.9-aarch64-apple-darwin.app.tar.gz']) assert.throws(()=>objectUrl('0.1.10',name));
});
