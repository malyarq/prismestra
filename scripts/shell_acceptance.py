"""Check normalized process and visible-window evidence from a disposable Windows guest."""

import argparse
import json
import re
from pathlib import Path, PureWindowsPath


SCENARIOS = {
    'setup', 'portable', 'logon', 'rgb-recovery', 'upgrade', 'uninstall', 'legacy-migration',
}
SHELLS = {
    'powershell.exe', 'powershell_ise.exe', 'pwsh.exe', 'cmd.exe',
    'sh.exe', 'bash.exe', 'dash.exe', 'ash.exe', 'zsh.exe', 'ksh.exe', 'wsl.exe',
}
CONSOLE_CLASSES = {'consolewindowclass', 'cascadia_hosting_window_class'}
SHA = re.compile(r'[0-9a-f]{40}')
SHA256 = re.compile(r'[0-9a-f]{64}')
PROCESS_ID = re.compile(r'[1-9][0-9]*:[1-9][0-9]*')


def index_processes(processes):
    index = {}
    for process in processes:
        identity = process['id']
        parent = process['parent']
        if (not PROCESS_ID.fullmatch(identity) or identity in index
                or parent is not None and not PROCESS_ID.fullmatch(parent)
                or not PureWindowsPath(process['image']).name):
            raise ValueError(f'Invalid process instance {identity!r}')
        index[identity] = process
    return index


def family(identity, processes, product_roots, harness_roots):
    seen = set()
    harness = False
    while identity in processes:
        if identity in product_roots:
            return 'product'
        if identity in harness_roots:
            harness = True
        if identity in seen:
            raise ValueError(f'Process ancestry cycle at {identity}')
        seen.add(identity)
        identity = processes[identity]['parent']
    return 'harness' if harness else 'unattributed'


def check_trace(trace, revision, artifact):
    if not SHA.fullmatch(revision) or not SHA256.fullmatch(artifact):
        raise ValueError('Expected a full revision and a lowercase SHA-256')
    scenario = trace['scenario']
    if scenario not in SCENARIOS:
        raise ValueError(f'Unknown scenario {scenario!r}')
    processes = index_processes(trace['processes'])
    product_roots = set(trace['product_roots'])
    harness_roots = set(trace['harness_roots'])
    gaps = []
    violations = []
    exact_artifact = trace['revision'] == revision and trace['artifact_sha256'] == artifact
    if not exact_artifact:
        gaps.append('Trace revision or artifact differs from the requested build')
    if not product_roots or not product_roots <= processes.keys():
        gaps.append('One or more product process roots were not captured')
    if not harness_roots <= processes.keys():
        gaps.append('One or more harness process roots were not captured')
    if product_roots & harness_roots:
        raise ValueError('Product and harness root sets overlap')
    for control in ('process_start_stop', 'visible_console', 'full_scenario'):
        if trace['controls'].get(control) is not True:
            gaps.append(f'Missing capture control: {control}')

    def attribute(identity):
        return family(identity, processes, product_roots, harness_roots)

    for identity, process in processes.items():
        image = PureWindowsPath(process['image']).name.casefold()
        if image in SHELLS:
            owner = attribute(identity)
            if owner == 'product':
                violations.append(f'Product started {image} ({identity})')
            elif owner != 'harness':
                gaps.append(f'Unattributed shell {image} ({identity})')
    for window in trace['windows']:
        if window.get('visible') is not True or window['class'].casefold() not in CONSOLE_CLASSES:
            continue
        identity = window['process']
        owner = attribute(identity)
        if owner == 'product':
            violations.append(f'Visible product console ({identity})')
        elif owner != 'harness':
            gaps.append(f'Unattributed visible console ({identity})')

    return {
        'scenario': scenario,
        'revision': revision,
        'artifact_sha256': artifact,
        'status': 'fail' if violations and exact_artifact else 'inconclusive' if gaps else 'pass',
        'violations': violations,
        'gaps': gaps,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--artifact-sha256', required=True)
    args = parser.parse_args()
    try:
        trace = json.loads(args.trace.read_text(encoding='utf-8-sig'))
        result = check_trace(trace, args.revision, args.artifact_sha256)
    except (OSError, KeyError, TypeError, ValueError) as error:
        parser.exit(2, f'Invalid trace: {error}\n')
    print(json.dumps(result, indent=2))
    return {'pass': 0, 'fail': 1, 'inconclusive': 2}[result['status']]


if __name__ == '__main__':
    raise SystemExit(main())
