import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parents[1]
if os.name != 'nt':
    raise SystemExit('Windows fixture requires the desktop build')
build = subprocess.run(['cargo', 'build', '--locked', '--lib', '--message-format=json'], cwd=root, check=True, capture_output=True, text=True, encoding='utf-8')
libs = [Path(path) for line in build.stdout.splitlines() for item in [json.loads(line)]
        if item.get('reason') == 'compiler-artifact' and item['target']['name'] == 'tg_ws_proxy_plus'
        for path in item['filenames'] if path.endswith('.rlib')]
assert libs
with tempfile.TemporaryDirectory(prefix='tgws-update-') as temporary:
    work = Path(temporary)
    for name, extra in [('good', []), ('bad', ['--cfg', 'fail_start'])]:
        subprocess.run(['rustc', '--edition=2024', str(root / 'tests/support/update_fixture.rs'),
                        '--extern', 'tg_ws_proxy_plus=' + str(libs[0]), '-L', 'dependency=' + str(root / 'target/debug/deps'),
                        '-C', 'target-feature=+crt-static', '-o', str(work / (name + '.exe')), *extra], check=True, cwd=root)
    for scenario in ['good', 'bad']:
        folder = work / scenario
        folder.mkdir()
        target = folder / 'app.exe'
        staged = folder / 'new.exe'
        config = folder / 'config.json'
        shutil.copyfile(work / 'good.exe', target)
        shutil.copyfile(work / (scenario + '.exe'), staged)
        config.write_text('{}', encoding='utf-8')
        original = target.read_bytes()
        expected = staged.read_bytes()
        digest = 'sha256:' + hashlib.sha256(expected).hexdigest()
        subprocess.run([str(target), '--parent', str(staged), str(config), digest], check=True, timeout=10)
        marker = folder / ('healthy.txt' if scenario == 'good' else 'rollback.txt')
        deadline = time.monotonic() + 45
        while not marker.exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        assert marker.exists(), list(folder.iterdir())
        time.sleep(4)
        assert target.read_bytes() == (expected if scenario == 'good' else original)
        if scenario == 'good':
            backups = list(folder.glob('*.backup.exe'))
            assert len(backups) == 1 and backups[0].read_bytes() == original
            assert not list(folder.glob('TgWsProxyPlus-install-*.json'))
            assert not list(folder.glob('TgWsProxyPlus-install-*.ready'))
            assert not [p for p in folder.glob('TgWsProxyPlus-install-*.exe') if not p.name.endswith('.backup.exe')]
        else:
            assert staged.read_bytes() == expected
            assert 'previous executable restored' in (folder / 'install-error.txt').read_text(encoding='utf-8')
        print(scenario + ': replacement, process lifecycle and ' + ('health confirmation' if scenario == 'good' else 'rollback') + ' passed')
