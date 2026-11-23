"""Compare cargo llvm-cov JSON reports from a test-only change."""

import argparse
import json
from pathlib import Path


def covered_regions(path):
    data, = json.loads(path.read_bytes())["data"]
    files = {entry["filename"] for entry in data["files"]}
    regions = set()
    for function in data["functions"]:
        for start, col, end, end_col, hits, file_id, _, kind in function["regions"]:
            filename = function["filenames"][file_id]
            # LLVM kind 0 denotes code. The file report applies the exclusions.
            if kind == 0 and hits and filename in files:
                regions.add((filename, start, col, end, end_col))
    if not regions:
        raise ValueError(f"{path} contains no covered code regions")
    lines = data["totals"]["lines"]
    print(f"{path}: {lines['covered']}/{lines['count']} lines ({lines['percent']:.2f}%)")
    return regions


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    args = parser.parse_args()
    lost = covered_regions(args.before) - covered_regions(args.after)
    for filename, start, col, end, end_col in sorted(lost):
        print(f"{filename}:{start}:{col}-{end}:{end_col}")
    print(f"{len(lost)} previously covered source regions lost")
    raise SystemExit(bool(lost))
