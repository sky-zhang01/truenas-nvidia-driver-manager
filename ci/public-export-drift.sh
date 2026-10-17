#!/usr/bin/env bash
# Report path membership changes; content safety is checked separately.
set -Eeuo pipefail
set +x
node - "$@" <<'NODE'
const fs = require('node:fs');
const {execFileSync} = require('node:child_process');
const {createHash} = require('node:crypto');
const args = process.argv.slice(2);
if (args.length !== 2) {
    console.error('usage: public-export-drift.sh <base-commit> <head-manifest>');
    process.exit(2);
}
function git(...values) {
    return execFileSync('git', ['--no-replace-objects', ...values], {encoding:'utf8', stdio:['ignore','pipe','pipe']});
}
try {
    const base = git('rev-parse','--verify','--end-of-options',args[0]+'^{commit}').trim();
    const manifest = JSON.parse(fs.readFileSync(args[1],'utf8'));
    const rules = git('show',base+':export/rules').split(/\r?\n/).map(line=>line.trim())
        .filter(line=>line && !line.startsWith('#')).map(line=>{
            const fields=line.split(/\s+/);
            if (fields.length!==2 || !['include','exclude'].includes(fields[0]) ||
                /[?*\[\]\\]/.test(fields[1]) || fields[1].startsWith('/') ||
                fields[1].split('/').some(segment=>segment==='.' || segment==='..')) throw new Error();
            return fields;
        });
    const entries=git('ls-tree','-r','-z','--name-only',base).split('\0').filter(Boolean);
    let unclassified=0;
    const before=new Set(entries.filter(name=>{
        const matches=rules.filter(([,rule])=>rule.endsWith('/')?name.startsWith(rule):name===rule).map(([action])=>action);
        if (!matches.length) unclassified++;
        return matches.includes('include') && !matches.includes('exclude');
    }));
    if (!Array.isArray(manifest.files) || manifest.files.some(file=>typeof file.path!=='string')) throw new Error();
    const after=new Set(manifest.files.map(file=>file.path));
    // Never expose path values: this report deliberately receives no private policy.
    const safe=name=>'[path sha256:'+createHash('sha256').update(name).digest('hex')+']';
    const added=[...after].filter(name=>!before.has(name)).sort();
    const removed=[...before].filter(name=>!after.has(name)).sort();
    console.log('Public export membership: '+added.length+' added, '+removed.length+' removed');
    console.log('Base commit: '+base);
    if (unclassified) console.log('Base policy has '+unclassified+' unclassified paths; they are treated as denied for this comparison.');
    for (const name of added) console.log('+ '+safe(name));
    for (const name of removed) console.log('- '+safe(name));
    console.log('This comparison covers path membership only.');
} catch {
    console.error('public export drift: local base policy or manifest is unavailable or invalid');
    process.exit(1);
}
NODE
