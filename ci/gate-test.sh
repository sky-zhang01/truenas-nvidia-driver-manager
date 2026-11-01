#!/usr/bin/env bash
# Strict job-result contracts; no optional success may disguise a routing error.
set -Eeuo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "${ROOT}" <<'PY'
import os
from pathlib import Path
import subprocess
import sys
import tempfile
root=Path(sys.argv[1])
jobs={'controls':'CONTROLS','web':'WEB','compose':'COMPOSE','rust':'RUST','e2e':'E2E','image':'IMAGE','build_kit':'BUILD_SCRIPT','osv':'OSV','leak':'LEAK'}
base={key:value for key,value in os.environ.items() if not (key.endswith('_RESULT') or key.startswith('SWITCH_') or key.startswith('GATE_'))}
base.update(CHANGES_RESULT='success',GUARDS_RESULT='success')
for job,result in jobs.items():
    base['SWITCH_'+job.upper()]='true'
    base[result+'_RESULT']='success'
count=0

def case(name,updates,expected):
    global count
    env=base.copy(); env.update(updates)
    result=subprocess.run(['bash',str(root/'ci/gate.sh')],env=env,capture_output=True,text=True)
    assert result.returncode==expected,(name,result.returncode,result.stdout,result.stderr)
    print('PASS:',name); count+=1

case('every selected job succeeded',{},0)
off={}
for job,result in jobs.items():
    off['SWITCH_'+job.upper()]='false'; off[result+'_RESULT']='skipped'
case('every unselected job skipped',off,0)
for result in ('failure','cancelled','skipped',''):
    case('changes '+(result or 'missing'),{'CHANGES_RESULT':result},1)
    case('guards '+(result or 'missing'),{'GUARDS_RESULT':result},1)
for job,result in jobs.items():
    for status in ('failure','cancelled','skipped',''):
        case(job+' selected '+(status or 'missing'),{result+'_RESULT':status},1)
    case(job+' unexpected success',{**off,result+'_RESULT':'success'},1)
    for switch in ('','yes','TRUE'):
        case(job+' invalid switch '+repr(switch),{'SWITCH_'+job.upper():switch},1)
    for status in ('failure','cancelled',''):
        case(job+' unselected '+(status or 'missing'),{**off,result+'_RESULT':status},1)
case('changes failure with transitive skipped jobs',{**off,'CHANGES_RESULT':'failure'},1)
case('Rust may skip while e2e succeeds',{'SWITCH_RUST':'false','RUST_RESULT':'skipped'},0)
case('e2e may skip while Rust succeeds',{'SWITCH_E2E':'false','E2E_RESULT':'skipped'},0)
case('PR head without live base fails closed',{'GATE_PR_HEAD':'branch-head'},1)
with tempfile.TemporaryDirectory() as work:
    fake=Path(work)/'git'
    fake.write_text('#!/bin/sh\n[ "$1" = merge-base ] && [ "$2" = --is-ancestor ] && [ "$3" = live-main ] && [ "$4" = actual-pr-head ] || exit 2\nexit "${ANCESTOR_STATUS:-0}"\n')
    fake.chmod(0o755)
    params={'PATH':work+os.pathsep+os.environ['PATH'],'GATE_PR_HEAD':'actual-pr-head','GATE_BASE_SHA':'live-main'}
    case('fresh PR head passes ancestor check',params,0)
    case('stale PR head fails despite merge-ref HEAD',{**params,'ANCESTOR_STATUS':'1'},1)
    case('unreadable ancestor check fails closed',{**params,'ANCESTOR_STATUS':'2'},1)
print(f'gate-test: {count} controls passed')
PY
