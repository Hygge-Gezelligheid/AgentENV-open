#!/usr/bin/env python3
"""Paired cluster bootstrap: resample complete trials, never individual saves."""
import argparse
import json
import random
from pathlib import Path


def quantile(values, probability):
    values = sorted(values)
    position = (len(values) - 1) * probability
    low = int(position)
    high = min(low + 1, len(values) - 1)
    return values[low] + (values[high] - values[low]) * (position - low)


def ratios(pairs, iterations, seed):
    def statistics(rows):
        return dict(p50=quantile(rows, 0.5), p95=quantile(rows, 0.95), mean=sum(rows)/len(rows))
    def compare(indices):
        baseline = statistics([value for index in indices for value in pairs[index][0]])
        candidate = statistics([value for index in indices for value in pairs[index][1]])
        return {name: candidate[name] / baseline[name] for name in baseline}
    point = compare(range(len(pairs)))
    samples = {name: [] for name in point}
    rng = random.Random(seed)
    for _ in range(iterations):
        draw = compare([rng.randrange(len(pairs)) for _ in pairs])
        for name, value in draw.items():
            samples[name].append(value)
    return {name: dict(ratio=point[name], upper_95_one_sided=quantile(samples[name], 0.95),
        interval_95=[quantile(samples[name], 0.025), quantile(samples[name], 0.975)]) for name in point}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('batch', type=Path)
    parser.add_argument('--iterations', type=int, default=10000)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    protocol = json.loads((args.batch/'protocol.json').read_text())
    grouped = {}
    for path in sorted(args.batch.glob('*/samples.jsonl')):
        rows = [json.loads(line) for line in path.read_text().splitlines()]
        if not rows or rows[-1]['kind'] != 'complete':
            raise ValueError(f'Incomplete trial: {path}')
        first = rows[0]
        key = (first['memory_mib'], first['scenario'], first['generations'])
        pair = grouped.setdefault(key, {}).setdefault(first['pair'], {})
        if first['variant'] in pair:
            raise ValueError(f'Duplicate paired variant: {path}')
        saves = [row for row in rows if row['kind'] == 'save']
        if len(saves) != first['generations'] or sorted(row['generation'] for row in saves) != list(range(first['generations'])):
            raise ValueError(f'Missing generations: {path}')
        measurements = {metric: [row[metric] for row in saves] for metric in ('save_ms', 'paused_upper_ms')}
        measurements['chain_save_ms'] = [sum(measurements['save_ms'])]
        measurements['chain_paused_ms'] = [sum(measurements['paused_upper_ms'])]
        for mode in ('warm', 'cold_snapshot_files'):
            restored = [row for row in rows if row['kind'] == 'restore' and row.get('cache_mode') == mode]
            if len(restored) != 1:
                raise ValueError(f'Missing or duplicate {mode} restore: {path}')
            for metric in ('ready_ms', 'first_read_ms', 'ready_and_read_ms'):
                measurements[mode+'_'+metric] = [restored[0][metric]]
        last = saves[-1]
        measurements['repository_bytes'] = [last['repository']['logical_bytes']]
        measurements['state_growth_bytes'] = [last['state']['allocated_bytes']-last['input_state']['allocated_bytes']]
        if 'retained_referenced_layers' in rows[-1]:
            measurements['retained_referenced_layers_bytes'] = [rows[-1]['retained_referenced_layers']['logical_bytes']]
            measurements['retained_referenced_layer_growth_bytes'] = [
                rows[-1]['retained_referenced_layers']['logical_bytes'] - rows[-1]['input_referenced_layers']['logical_bytes']]
        pair[first['variant']] = measurements
    planned = {(int(memory), scenario, int(generations))
        for memory, scenario, generations in (case.split(':') for case in protocol['cases'])}
    if set(grouped) != planned:
        raise ValueError('Missing planned workload cells')
    reports = []
    for cell_index, (key, cells) in enumerate(sorted(grouped.items())):
        if set(cells) != set(range(protocol['pairs'])) or any(set(pair) != {'baseline', 'candidate'} for pair in cells.values()):
            raise ValueError(f'Incomplete paired workload: {key}')
        report = dict(memory_mib=key[0], scenario=key[1], generations=key[2], pairs=len(cells), metrics={})
        for metric in next(iter(cells.values()))['baseline']:
            paired = [(pair['baseline'][metric], pair['candidate'][metric]) for _, pair in sorted(cells.items())]
            result = ratios(paired, args.iterations, 20261107+cell_index)
            if metric.endswith('_bytes'):
                result['space_upper_below_one'] = result['mean']['upper_95_one_sided'] < 1
            else:
                result['typical_pass_5_percent'] = result['p50']['upper_95_one_sided'] <= 1.05
                result['tail_pass_10_percent'] = result['p95']['upper_95_one_sided'] <= 1.10
            report['metrics'][metric] = result
        report['enough_independent_pairs'] = len(cells) >= 30
        report['performance_confirmed'] = report['enough_independent_pairs'] and all(
            result['typical_pass_5_percent'] and result['tail_pass_10_percent']
            for metric, result in report['metrics'].items() if not metric.endswith('_bytes'))
        reports.append(report)
    result = dict(bootstrap_iterations=args.iterations, bootstrap_unit='independent paired whole trial',
        noninferiority_margin_p50=0.05, noninferiority_margin_p95=0.10,
        protocol=protocol, workloads=reports,
        all_workload_performance_confirmed=all(row['performance_confirmed'] for row in reports))
    args.output.write_text(json.dumps(result, indent=2))
    print(json.dumps({key: value for key, value in result.items() if key not in ('protocol', 'workloads')}))


if __name__ == '__main__':
    main()
