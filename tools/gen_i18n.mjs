import fs from 'node:fs';
const src = fs.readFileSync(process.argv[2], 'utf8') + '\n' + fs.readFileSync(process.argv[3], 'utf8');
const fn = new Function(src + '\nreturn {LANGS, D};');
const { LANGS, D } = fn();
fs.writeFileSync(process.argv[4], JSON.stringify({ langs: LANGS, d: D }));
console.log('langs', LANGS.length, 'keys', Object.keys(D.en).length);
