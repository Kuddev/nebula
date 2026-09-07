"""Size a DMG for logical file contents, including sparse/copied binaries."""

import argparse
import os
import stat
from pathlib import Path


MIB = 1024 * 1024
BLOCK = 4096


def image_size_mib(source: Path) -> int:
    if source.is_symlink() or not source.is_dir():
        raise ValueError(f"DMG source must be a directory: {source}")
    total = 0

    def fail(error):
        raise error

    # Count every directory entry without following /Applications or deduplicating
    # hard links: the destination copy may materialize each logical file.
    for directory, directories, files in os.walk(source, onerror=fail):
        for name in directories + files:
            entry = (Path(directory) / name).lstat()
            if not (stat.S_ISREG(entry.st_mode) or stat.S_ISDIR(entry.st_mode)
                    or stat.S_ISLNK(entry.st_mode)):
                raise ValueError(f"Unsupported DMG input: {Path(directory) / name}")
            total += ((entry.st_size + BLOCK - 1) // BLOCK + 1) * BLOCK

    # Allow 25% growth plus 32 MiB for the filesystem, catalog and journal.
    return max(64, (total * 5 + 4 * MIB - 1) // (4 * MIB) + 32)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    print(image_size_mib(args.source))
