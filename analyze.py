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
git grep -A1 'tokio'::main | grep 'fn ' > tokio_mains.txt

# And then
./analyze.py index_scip.json call_graph.json tokio_mains.txt

"""

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

    # print(mains)

    nodes = graph['nodes']
    edges = graph['links']

    bad_funcs = set()

    # Try to match the function names we have with symbols
    for main in mains:
        filename, rest = main.split('-', 1)
        stem = filename.replace('src/', '').replace('.rs', '')  # do it better?
        if stem.endswith('/mod'):
            stem = stem[:-len('/mod')]
        funcname = rest.strip().split('fn', 1)[1].strip().split('(')[0]

        for node in nodes:
            symbol = node['symbol']
            if (
                f'{stem}/impl#' in symbol
                and symbol.endswith(f']{funcname}().')
            ) or symbol.endswith(f'{stem}/{funcname}().'):
                # print(stem, funcname, "->", symbol)
                bad_funcs.add(symbol)
                break
        else:
                print('MISSED', stem, funcname)


    async_funcs = set()
    # We have to look through the original scip data to get symbols
    for doc in scip_data['documents']:
        for symbol in doc['symbols']:
            sig = symbol.get('signature_documentation')
            if sig and 'async fn' in sig.get('text'):
                async_funcs.add(symbol['symbol'])

    # for node in nodes:
    #     if 'async fn' in node.get('body', ''):
    #         name = node['symbol']
    #         if name not in async_funcs:
    #             if name not in bad_funcs:
    #                 print("SEARCH IS MISSING", name)
    #         async_funcs.add(name)

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
            # need to do the checks at the outbound side, not the inbound one,
            # because we need to tell if the bad functions are getting *called*
            if tgt not in async_called:
                async_called[tgt] = s
                wl.append(tgt)

    print(f'{len(bad_funcs)=}')
    print(f'{len(async_funcs)=}')
    print(f'{len(async_called)=}')
    print(f'{len(async_called.keys() | async_funcs)=}')

    danger = async_called.keys() & bad_funcs

    # TODO: we only generate one bad path; doing multiple could be better!!
    print()
    for bad in sorted(danger):
        print(trim_symbol(bad))

        n = bad
        path = [n]
        while n in async_called:
            n = async_called[n]
            path.append(n)
        print([trim_symbol(s) for s in path])



if __name__ == '__main__':
    main(sys.argv)
