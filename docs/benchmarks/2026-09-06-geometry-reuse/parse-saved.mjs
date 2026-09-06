import {readFileSync} from 'node:fs';
import {parse,audit} from './diagnostic-parser.mjs';
const spec=JSON.parse(readFileSync(process.argv[2],'utf8'));
const output=readFileSync(spec.log,'utf8');
const parsed=parse(output), issues=audit(parsed,spec,spec);
console.log(JSON.stringify({parsed,issues,acceptance_eligible:false}));
