#!/usr/bin/env python3
"""Run serial, randomized paired API trials with fresh repositories and state."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import signal
import subprocess
import sys
import time
import urllib.request


def digest(path):
    hasher = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            hasher.update(chunk)
    return hasher.hexdigest()


def build_seed(root, destination):
    source = root / 'hardware/home/snapshot-store/repository'
    destination.mkdir(parents=True)
    layers = set()
    seeds = {}
    def references(value):
        if isinstance(value, dict):
            if isinstance(value.get('digest'), str) and value['digest'].startswith('sha256:'):
                layers.add(value['digest'])
            for child in value.values():
                references(child)
        elif isinstance(value, list):
            for child in value:
                references(child)
    for memory in (512, 4096):
        info = json.loads((root / f'seed-{memory}.json').read_text())
        snapshot = info['snapshot_id']
        seeds[str(memory)] = snapshot
        record = source / 'catalog/records' / (snapshot + '.json')
        references(json.loads(record.read_text())['committed'])
        target = destination / 'catalog/records' / record.name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(record, target)
        shutil.copytree(source / 'snapshots' / snapshot, destination / 'snapshots' / snapshot)
    (destination / 'managed-layers').mkdir()
    for layer in sorted(layers):
        name = layer.replace(':', '_') + '.overlaybd.commit'
        os.link(source / 'managed-layers' / name, destination / 'managed-layers' / name)
    return seeds


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True, type=Path)
    parser.add_argument('--checkout', required=True, type=Path)
    parser.add_argument('--label', required=True)
    parser.add_argument('--pairs', type=int, default=30)
    parser.add_argument('--cases', nargs='+', default=[
        '512:idle:8', '512:rotating:8', '512:hotspot:8', '512:dense:8',
        '4096:rotating:8', '4096:dense:8', '512:rotating:96'])
    args = parser.parse_args()
    root = args.root.resolve()
    batch = root / ('comparison-' + args.label)
    batch.mkdir()
    seeds = build_seed(root, batch / 'seed-repository')
    script_dir = Path(__file__).resolve().parent
    binaries = {variant: root / 'bin' / variant / 'server' for variant in ('baseline', 'candidate')}
    daemons = {variant: root / 'bin' / variant / 'uvm-ublk-daemon' for variant in binaries}
    common = dict(firecracker=root/'runtime-target/x86_64-unknown-linux-musl/release/firecracker',
        kernel=root/'hardware/deps/kernel/vmlinux-6.18/vmlinux.bin')
    rng = random.Random(20261107)
    order = []
    for case, spec in enumerate(args.cases):
        memory, scenario, generations = spec.split(':')
        for pair in range(args.pairs):
            variants = list(binaries)
            rng.shuffle(variants)
            for variant in variants:
                order.append(dict(case=case, memory=int(memory), scenario=scenario,
                    generations=int(generations), pair=pair, variant=variant))
    protocol = dict(random_seed=20261107, pairs=args.pairs, cases=args.cases, order=order,
        executables={key: dict(path=str(path), sha256=digest(path))
            for key, path in {**binaries, **{key + '_daemon': path for key, path in daemons.items()},
                **common}.items()},
        drivers={path.name: digest(path) for path in (
            script_dir/'run_clear_comparison.py', script_dir/'clear_api_trial.py', script_dir/'guest_process_rpc.py')},
        tools_input=dict(path=str(root/'hardware/deps/tools/0.1.1/tools.ext4'),
            bytes=(root/'hardware/deps/tools/0.1.1/tools.ext4').stat().st_size),
        seeds=seeds, state_retention='Keep first-pair and failed-trial state and small runtime directories; reclaim other successful home/work state after recording.', exclusions='Warm-up/setup and output collection are outside timing; exploratory runs are separate.')
    (batch/'protocol.json').write_text(json.dumps(protocol, indent=2))
    prefix = hashlib.sha256(args.label.encode()).hexdigest()[:4]
    for index, cell in enumerate(order):
        identifier = f'{prefix}{index:04x}'
        trial = batch / f"{index:04}-{cell['variant']}"
        trial.mkdir()
        home = trial / 'home'
        repository = home / 'snapshot-store/repository'
        repository.parent.mkdir(parents=True)
        def copy_seed_file(source, destination):
            if 'managed-layers' in Path(source).parts:
                return os.link(source, destination)
            return shutil.copy2(source, destination)
        shutil.copytree(batch/'seed-repository', repository, copy_function=copy_seed_file)
        runtime = root / 'r' / identifier
        work = root / 'w' / identifier
        serial = trial / 'serial'
        config = trial / 'node.toml'
        config.write_text(f'''home_path = "{home}"
runtime_path = "{runtime}"
deps_path = "{root}/hardware/deps"
[firecracker]
binary_path = "{common['firecracker']}"
work_dir = "{work}"
serial_dir = "{serial}"
log_level = "Info"
[kernel]
image_path = "{common['kernel']}"
[backend.posix_fs]
snapshot_store = "{home}/snapshot-store"
[p2p]
enabled = false
[network.internal]
host_interaction_cidr = "10.244.0.0/16"
veth_cidr = "10.245.0.0/16"
[ublk]
daemon_binary_path = "{daemons[cell['variant']]}"
daemon_metrics_listen_addr = ""
[pool.network]
enabled = false
[pool.block]
enabled = false
[observability.scheduler_report]
enabled = false
''')
        environment = dict(os.environ, AENV_CONFIG_PATH=str(config), AENV_HOME_PATH=str(home),
            AENV_RUNTIME_PATH=str(runtime), API_ADDR='127.0.0.1:18971', RUST_LOG='warn')
        server_log = (trial/'server.log').open('wb')
        command = [str(args.checkout/'scripts/run-with-capabilities.sh'), str(binaries[cell['variant']])]
        process = subprocess.Popen(command, cwd=args.checkout, env=environment,
            stdout=server_log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + 120
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f'Server exited: {trial}/server.log')
                key_path = home/'secrets/api-key'
                try:
                    key = key_path.read_text().strip()
                    request = urllib.request.Request('http://127.0.0.1:18971/sandboxes', headers={'X-API-Key': key})
                    with urllib.request.urlopen(request, timeout=1) as response:
                        if response.status == 200:
                            break
                except (OSError, ValueError):
                    pass
                if time.monotonic() > deadline:
                    raise RuntimeError(f'Server readiness deadline: {trial}/server.log')
                time.sleep(0.1)
            time.sleep(1)
            trial_command = [sys.executable, str(script_dir/'clear_api_trial.py'),
                '--url', 'http://127.0.0.1:18971', '--key-file', str(key_path),
                '--template', seeds[str(cell['memory'])], '--state-root', str(home),
                '--repository-root', str(repository), '--runtime-root', str(work),
                '--serial-dir', str(serial), '--output', str(trial/'samples.jsonl'),
                '--variant', cell['variant'], '--pair', str(cell['pair']),
                '--memory-mib', str(cell['memory']), '--generations', str(cell['generations']),
                '--scenario', cell['scenario']]
            with (trial/'driver.log').open('wb') as log:
                subprocess.run(trial_command, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=3600)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=60)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            server_log.close()
        if cell['pair'] > 0:
            # Runtime directories can have a root-owned parent created by host
            # setup. They are small; retain them rather than using privileged
            # recursive deletion in the measurement runner.
            for owned_path in (home, work):
                if not owned_path.resolve().is_relative_to(root):
                    raise RuntimeError('Refusing to reclaim state outside the task root')
                if owned_path.exists():
                    shutil.rmtree(owned_path)
            (trial/'state-reclaimed.json').write_text(json.dumps(dict(verified=True)))
        with (batch/'progress.jsonl').open('a') as progress:
            progress.write(json.dumps(dict(index=index, **cell, complete=True))+'\n')
        print(json.dumps(dict(index=index, **cell, complete=True)), flush=True)
    (batch/'complete.json').write_text(json.dumps(dict(trials=len(order), complete=True)))


if __name__ == '__main__':
    main()
