// Local preparation only; refuses to replace an existing reviewed seal.
import {lstatSync,readFileSync,writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {ROOT,LOCKED_PATHS,sha,verifyLocks} from './contract.mjs';
const locks={};
for(const path of [...LOCKED_PATHS].sort()){
 const file=join(ROOT,path),stat=lstatSync(file);
 if(!stat.isFile()||stat.isSymbolicLink())throw Error('Regular reviewed file required: '+path);
 locks[path]=sha(readFileSync(file));
}
writeFileSync(join(ROOT,'helpers-lock.json'),JSON.stringify(locks,null,2)+'\n',{flag:'wx',mode:0o600});
verifyLocks();
console.log(JSON.stringify({status:'sealed-not-dispatched',files:Object.keys(locks).length}));
