#!/usr/bin/env python3

import json
import sys

"""
# This uses some weird deps:
# https://github.com/sourcegraph/scip  (seems basically plausible)
# https://github.com/Beneficial-AI-Foundation/scip-callgraph  (was a pain in my ass but probably saved some time)

# Prep commands
rust-analyzer scip .
~/tmp/scip print --json index.scip > index_scip.json
~/tmp/scip-callgraph/target/debug/export_call_graph_d3 -- ~/src/e/edgedb-cli/index_scip.json && mv call_graph_d3.json call_graph.json
git grep -n -A1 'tokio'::main src > tokio_mains.txt

# And then
./analyze.py index_scip.json call_graph.json tokio_mains.txt

"""

EXEMPT = {
    'portable/windows/_get_wsl_distro().',
}

def trim_symbol(s):
    return s.split(' ')[-1]


def main(args):
    _, scipfile, jfile, mains_file = args

    with open(scipfile) as f:
        scip_data = json.load(f)

    with open(jfile) as f:
        graph = json.load(f)

    with open(mains_file) as f:
        mains = [s.strip() for s in f]

    nodes = graph['nodes']
    edges = graph['links']

    bad_funcs = set()

    documents = {doc['relative_path'].replace('\\', '/'): doc for doc in scip_data['documents']}

    # Match the line numbers with symbol definitions
    for main in mains:
        if 'tokio''::main' not in main:
            continue
        filename, num, rest = main.split(':', 2)
        num = int(num)
        doc = documents[filename]
        for symbol in doc['occurrences']:
            if symbol.get('symbol_roles', 0) & 1 and symbol['range'][0] == num:
                # print(filename, num, "->", symbol['symbol'])
                bad_funcs.add(symbol['symbol'])
                break
        else:
                print('MISSED', filename, num)

    async_funcs = set()
    # We have to look through the original scip data to get symbols
    for doc in scip_data['documents']:
        for symbol in doc['symbols']:
            sig = symbol.get('signature_documentation')
            if sig and 'async fn' in sig.get('text'):
                async_funcs.add(symbol['symbol'])

    # The tokio::main "bad functions" are async but don't have it in their signature anymore!
    async_funcs.update(bad_funcs)

    sgraph = dict()
    for edge in edges:
        sgraph.setdefault(edge["source"], []).append(edge["target"])

    # print(bad_funcs)
    # print(async_funcs)
    # print(sgraph)

    async_called = {}
    wl = list(async_funcs)
    while wl:
        s = wl.pop()
        for tgt in sgraph.get(s, ()):
            if trim_symbol(tgt) in EXEMPT:
                continue
            # need to do the checks at the outbound side, not the inbound one,
            # because we need to tell if the bad functions are getting *called*
            if tgt not in async_called:
                async_called[tgt] = s
                wl.append(tgt)

    danger = async_called.keys() & bad_funcs

    print(f'{len(bad_funcs)=}')
    print(f'{len(async_funcs)=}')
    print(f'{len(async_called)=}')
    print(f'{len(async_called.keys() | async_funcs)=}')
    print(f'{len(danger)=}')

    # TODO: we only generate one bad path; doing multiple could be better!!
    for bad in sorted(danger):
        print()
        print(trim_symbol(bad))

        n = bad
        path = [n]
        while n in async_called:
            n = async_called[n]
            if n in path:
                break
            path.append(n)
            if n in async_funcs:
                break
        print([trim_symbol(s) for s in path])



if __name__ == '__main__':
    main(sys.argv)
