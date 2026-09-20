"""Component feasibility probe; not the Stylon release acceptance benchmark.

Usage: python3 benchmarks/readiness/run.py /path/to/battlement
Requires Python 3.11+, Rust 1.98+, and Git. Only a temporary corpus is modified.
Build/downloads and corpus export are outside the measured intervals.
"""
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time

REVISION = '725660cccf08e66d2151ad5c5566fc4245e7070d'
HERE = Path(__file__).resolve().parent


def run(args, cwd=None):
    return subprocess.check_output(args, cwd=cwd, text=True)


def measure(commands, cwd):
    samples = []
    for iteration in range(23):
        start = time.perf_counter()
        for command in commands:
            output = run(command, cwd)
        elapsed = time.perf_counter() - start
        if iteration >= 3:
            samples.append(elapsed)
    return {'runs_seconds': samples, 'median_seconds': statistics.median(samples),
            'p95_seconds': sorted(samples)[18]}, output


def main():
    repository = Path(sys.argv[1]).resolve()
    git = ['git', '-C', str(repository)]
    with tempfile.TemporaryDirectory(prefix='stylon-readiness-') as temporary:
        work = Path(temporary)
        corpus = work / 'corpus'
        corpus.mkdir()
        names = run(git + ['ls-tree', '-rz', '--name-only', REVISION]).split('\0')
        for name in names:
            if not (name.endswith(('.rs', 'Cargo.toml', 'Cargo.lock', '.gitignore'))
                    or (name.endswith('.toml') and '/.cargo/' in '/' + name)):
                continue
            target = corpus / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(subprocess.check_output(git + ['show', REVISION + ':' + name]))
        sources = sorted(corpus.rglob('*.rs'))
        paths = work / 'paths.txt'
        paths.write_text(''.join(str(p) + '\n' for p in sources))
        run(['cargo', 'build', '--release', '--locked', '--manifest-path',
             str(HERE / 'Cargo.toml'), '--target-dir', str(work / 'target')])
        binary = work / 'target/release/stylon-readiness'
        covered = set()
        invocations = []
        for manifest in sorted(corpus.rglob('Cargo.toml'), key=lambda p: (len(p.parts), str(p))):
            if str(manifest) in covered:
                continue
            data = json.loads(run(['cargo', 'metadata', '--no-deps', '--format-version', '1',
                                   '--frozen', '--manifest-path', str(manifest)], manifest.parent))
            covered.update(package['manifest_path'] for package in data['packages'])
            invocations.append(manifest)
        locks_before = {str(p): p.read_bytes() for p in corpus.rglob('Cargo.lock')}
        parse, counts = measure([[str(binary), str(paths)]], corpus)
        metadata, _ = measure([['cargo', 'metadata', '--no-deps', '--format-version', '1',
                                '--frozen', '--manifest-path', str(p)] for p in invocations], corpus)
        assert locks_before == {str(p): p.read_bytes() for p in corpus.rglob('Cargo.lock')}
        parse['counts'] = counts.strip()
        result = {'revision': REVISION, 'rustc': run(['rustc', '--version']).strip(),
                  'cargo': run(['cargo', '--version']).strip(),
                  'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                  'lock_sha256': hashlib.sha256((HERE / 'Cargo.lock').read_bytes()).hexdigest(),
                  'tracked_rust_lines': sum(len(p.read_bytes().splitlines()) for p in sources),
                  'metadata_invocations': [str(p.relative_to(corpus)) for p in invocations],
                  'parse_and_walk': parse, 'all_metadata': metadata}
        if sys.platform == 'darwin':
            result['macos'] = run(['sw_vers'])
            result['hardware'] = run(['sysctl', 'hw.model', 'hw.memsize', 'hw.ncpu', 'machdep.cpu.brand_string'])
            result['power_settings'] = run(['pmset', '-g', 'custom'])
        print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
