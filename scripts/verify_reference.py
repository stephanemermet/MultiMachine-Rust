#!/usr/bin/env python3
"""Read-only integrity check of the .NET reference on the conversion workstation."""
import hashlib, json, pathlib, subprocess
root = pathlib.Path(__file__).resolve().parents[1]
reference = json.loads((root / 'docs/reference-sha256.json').read_text())
source = pathlib.Path(reference['source_path'])
head = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
assert head == reference['source_commit'], 'The reference HEAD changed'
for name, expected in reference['files'].items():
    assert hashlib.sha256((source / name).read_bytes()).hexdigest() == expected, name
status = subprocess.check_output(['git', '-C', str(source), 'status', '--porcelain'], text=True)
assert not status, 'The reference working tree is not clean'
print(f"Reference intact: {len(reference['files'])} tracked files, {head}, clean working tree")
