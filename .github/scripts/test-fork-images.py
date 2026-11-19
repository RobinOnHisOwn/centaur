#!/usr/bin/env python3
"""Run with Python + PyYAML; no registry or cluster access."""
import json
import os
import subprocess
from pathlib import Path

import yaml

root = Path(__file__).resolve().parents[2]
workflow = yaml.safe_load((root / '.github/workflows/fork-images.yml').read_text())
# PyYAML's YAML 1.1 loader treats the GitHub Actions key 'on' as True.
events = workflow[True]
assert set(events) == {'push', 'workflow_dispatch'}
assert events['push']['branches'] == ['main']
job = workflow['jobs']['images']
assert "github.repository == 'RobinOnHisOwn/centaur'" in job['if']
assert job['runs-on'] == 'self-hosted'
assert workflow['permissions'] == {'contents': 'read', 'packages': 'write'}
assert {i['component'] for i in job['strategy']['matrix']['include']} == {
    'api-rs', 'console', 'proxy-sync', 'token-broker', 'iron-proxy', 'agent'
}
steps = job['steps']
for revision, valid in [('a' * 40, True), ('main', False), ('a' * 39, False), ('$(exit 0)', False)]:
    result = subprocess.run(['bash', '-c', steps[0]['run']], env={**os.environ, 'REVISION': revision})
    assert (result.returncode == 0) is valid, revision
assert next(s for s in steps if 'uses' in s and 'checkout@' in s['uses'])['with']['persist-credentials'] is False
body = json.dumps(steps)
assert 'git rev-parse HEAD' in body
assert 'ghcr.io/${GITHUB_REPOSITORY,,}/centaur-' in body
assert 'org.opencontainers.image.source=https://github.com/$GITHUB_REPOSITORY' in body
assert 'RUST_BUILD_PROFILE=release' in body
assert 'buildah build' in body and 'buildah push' in body
assert 'kubectl' not in body and 'helm upgrade' not in body
assert steps[-1]['if'] == 'always()'
upstream = yaml.safe_load((root / '.github/workflows/publish-images.yml').read_text())
for name in ('build', 'merge', 'notify-centaur-paradigm'):
    assert "github.repository == 'paradigmxyz/centaur'" in upstream['jobs'][name]['if'], name
print('PASS: fork image ownership, trusted triggers, immutable tags, release builds and upstream isolation')
