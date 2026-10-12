#!/usr/bin/env python3
"""Fetch exact public sibling revisions for the experimental shared studio.

Requires Python 3.9+ and Git. Existing checkouts are verified, never reset.
Run from a checkout named craft-launcher. --check performs no network writes.
"""
import argparse
import json
import re
import subprocess
from pathlib import Path


def git(directory, *args):
    return subprocess.check_output(['git', '-C', str(directory), *args], text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='verify existing clean sibling checkouts only')
    options = parser.parse_args()
    launcher = Path(__file__).resolve().parents[1]
    if launcher.name != 'craft-launcher':
        raise RuntimeError('Clone the contribution into a directory named craft-launcher; sibling path dependencies require that name.')
    pins = json.loads((launcher / 'studio-sources.json').read_text(encoding='utf-8'))['repositories']
    for pin in pins:
        name, url, revision = pin['directory'], pin['repository'], pin['revision']
        if not re.fullmatch(r'[a-z]+craft-(?:shell|mixer)', name):
            raise RuntimeError('Invalid sibling directory in manifest')
        if not re.fullmatch(r'https://github\.com/(?:storytold|durtyrobbin)/[a-z]+craft\.git', url):
            raise RuntimeError('Invalid public source URL in manifest')
        if not re.fullmatch(r'[0-9a-f]{40}', revision):
            raise RuntimeError('Source revisions must be immutable full commit IDs')
        destination = launcher.parent / name
        if not destination.exists():
            if options.check:
                raise RuntimeError(f'Missing checkout: {name}; run without --check first')
            destination.mkdir()
            git(destination, 'init', '--quiet')
            git(destination, 'remote', 'add', 'origin', url)
            git(destination, 'fetch', '--depth=1', 'origin', revision)
            git(destination, 'checkout', '--quiet', '--detach', revision)
        if git(destination, 'rev-parse', '--show-toplevel').replace('\\', '/').lower() != destination.resolve().as_posix().lower():
            raise RuntimeError(f'{name} must be a separate checkout, not part of another repository')
        if git(destination, 'rev-parse', 'HEAD') != revision:
            raise RuntimeError(f'{name} is at another revision; preserve your work and prepare a fresh sibling workspace')
        if git(destination, 'status', '--porcelain'):
            raise RuntimeError(f'{name} contains changes; preserve them before using it as a reproducible source')
        print(f'Verified {name}: {revision}', flush=True)


if __name__ == '__main__':
    main()
