#!/usr/bin/env python3
"""Package an already-built Windows binary, its instructions and dependency licenses."""
import argparse, hashlib, json, os, pathlib, shutil, subprocess, zipfile

root = pathlib.Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--target', default='')
p.add_argument('--cargo', default=os.environ.get('CARGO', 'cargo'))
a = p.parse_args()
metadata = json.loads(subprocess.check_output([a.cargo, 'metadata', '--locked', '--format-version', '1'], cwd=root))
package = next(p for p in metadata['packages'] if p['name'] == 'multimachine-rust')
version = package['version']
exe = root / 'target' / a.target / 'release' / 'multimachine-rust.exe'
if not exe.is_file():
    raise SystemExit(f'Build the Windows executable first: {exe}')
name = f'MultiMachineRust-{version}-windows-x64'
output = root / 'artifacts'
output.mkdir(exist_ok=True)
stage = output / name
if stage.exists():
    shutil.rmtree(stage)
stage.mkdir()
shutil.copy2(exe, stage / 'MultiMachineRust.exe')
for file in ['README.md', 'LICENSE', 'docs/VALIDATION.md', 'docs/PROTOCOL.md']:
    src = root / file
    dest = stage / file
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest)
notices = ['# Composants tiers', '', 'Dépendances verrouillées dans Cargo.lock ; leurs licences demeurent applicables.', '']
for pkg in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
    if pkg['name'] == 'multimachine-rust':
        continue
    notices += [f"## {pkg['name']} {pkg['version']}", '', f"Licence déclarée : {pkg['license'] or 'voir le texte fourni'}.", '']
    if pkg.get('repository'):
        notices += [f"Source : {pkg['repository']}", '']
    directory = pathlib.Path(pkg['manifest_path']).parent
    candidates = set()
    for pattern in ['LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*']:
        candidates.update(directory.glob(pattern))
    if pkg.get('license_file'):
        candidates.add(directory / pkg['license_file'])
    for source in sorted(candidates):
        if source.is_file():
            dest = stage / 'licenses' / f"{pkg['name']}-{pkg['version']}" / source.name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest)
            notices += [f'- [{source.name}]({dest.relative_to(stage).as_posix()})']
    notices += ['']
(stage / 'THIRD-PARTY-NOTICES.md').write_text('\n'.join(notices), encoding='utf-8')
zip_path = output / (name + '.zip')
with zipfile.ZipFile(zip_path, 'w', zipfile.ZIP_DEFLATED, compresslevel=9, strict_timestamps=False) as archive:
    for file in sorted(stage.rglob('*')):
        if file.is_file():
            archive.write(file, name + '/' + file.relative_to(stage).as_posix())
with zipfile.ZipFile(zip_path) as archive:
    if archive.testzip() is not None:
        raise SystemExit('ZIP verification failed')
digest = hashlib.sha256(zip_path.read_bytes()).hexdigest()
(zip_path.with_suffix('.zip.sha256')).write_text(f'{digest}  {zip_path.name}\n', encoding='ascii')
print(json.dumps({'zip': str(zip_path), 'bytes': zip_path.stat().st_size, 'sha256': digest}, indent=2))
