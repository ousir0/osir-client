// Ephemeral Windows runner only. Drives the real shipped updater UI.
import assert from 'node:assert/strict';
import {writeFileSync} from 'node:fs';
import {join} from 'node:path';
const [port, mode, directory, expected] = process.argv.slice(2);
assert.equal(process.env.GITHUB_ACTIONS, 'true');
assert.equal(process.platform, 'win32');
assert.ok(['prepare','update','verify'].includes(mode));
assert.match(port,/^\d+$/);
const delay=ms=>new Promise(r=>setTimeout(r,ms));
let socket, serial=0, pending=new Map();
async function connect(){
 const until=Date.now()+90000;
 while(Date.now()<until){
  try{
   const pages=await fetch(`http://127.0.0.1:${port}/json/list`).then(r=>r.json());
   const page=pages.find(p=>p.type==='page' && /^https?:\/\/tauri\.localhost\/(?:index.html)?$/.test(p.url));
   if(!page){await delay(500);continue;}
   const endpoint=new URL(page.webSocketDebuggerUrl); assert.equal(endpoint.hostname,'127.0.0.1');assert.equal(endpoint.port,port);
   socket=new WebSocket(endpoint);
   await new Promise((r,j)=>{socket.addEventListener('open',r,{once:true});socket.addEventListener('error',j,{once:true});});
   socket.addEventListener('message',({data})=>{const row=JSON.parse(String(data));if(row.id)pending.get(row.id)?.(row);});
   return;
  }catch{await delay(500);}
 }
 throw Error('Shipped application renderer did not become available');
}
function call(method,params={}){return new Promise((r,j)=>{const id=++serial;const timer=setTimeout(()=>{pending.delete(id);j(Error('CDP request timed out'));},15000);pending.set(id,row=>{clearTimeout(timer);pending.delete(id);row.error?j(Error(row.error.message)):r(row.result);});socket.send(JSON.stringify({id,method,params}));});}
async function evaluate(expression){const row=await call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});assert.ok(!row.exceptionDetails,'Native IPC failed');return row.result?.value;}
try{
 await connect();
 const version=await evaluate(`window.__TAURI_INTERNALS__.invoke('plugin:app|version')`);assert.equal(version,expected);
 const distribution=await evaluate(`window.__TAURI_INTERNALS__.invoke('get_update_distribution')`);assert.equal(distribution.kind,'osir');assert.equal(distribution.updatesEnabled,true);
 if(mode==='update'){
  let ready=false;
  for(let i=0;i<90;i++){
   ready=await evaluate(`Array.from(document.querySelectorAll('button')).some(b=>b.textContent.trim()==='更新并重启'&&!b.disabled)`);
   if(ready)break;await delay(1000);
  }
  assert.ok(ready,'Real update dialog did not offer installation');
  const before=await call('Page.captureScreenshot',{format:'png'});writeFileSync(join(directory,'updater-before.png'),Buffer.from(before.data,'base64'));
  const clicked=await evaluate(`(()=>{const b=Array.from(document.querySelectorAll('button')).find(b=>b.textContent.trim()==='更新并重启'&&!b.disabled);if(!b)return false;b.click();return true;})()`);assert.equal(clicked,true);
  writeFileSync(join(directory,'updater-click.json'),JSON.stringify({passed:true,version,realUiButton:'更新并重启',time:new Date().toISOString()},null,2));
  await delay(1000);
 }else{
  const state=await evaluate(`({ready:!!document.querySelector('.osir-welcome-login'),text:document.body.innerText.includes('授权登录OSIRAPI')})`);
  assert.equal(state.ready,true);assert.equal(state.text,true);
  const screenshot=await call('Page.captureScreenshot',{format:'png'});writeFileSync(join(directory,`updater-${mode}.png`),Buffer.from(screenshot.data,'base64'));
  writeFileSync(join(directory,`updater-${mode}.json`),JSON.stringify({passed:true,version,distribution,realRendererAndIpc:true},null,2));
 }
}finally{socket?.close();}
