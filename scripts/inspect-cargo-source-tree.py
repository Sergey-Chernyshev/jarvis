import hashlib
import json
import os
import stat
import sys

MAX_DEPTH = 64
MAX_ENTRIES = 20_000
MAX_FILE_BYTES = 32 * 1024 * 1024
MAX_TOTAL_BYTES = 256 * 1024 * 1024
STABLE_FIELDS = (
    "st_dev",
    "st_ino",
    "st_mode",
    "st_nlink",
    "st_size",
    "st_mtime_ns",
    "st_ctime_ns",
)


def same_identity(left, right):
    return all(getattr(left, field) == getattr(right, field) for field in STABLE_FIELDS)


def read_digest(file_descriptor, size):
    digest = hashlib.sha256()
    remaining = size
    while remaining:
        chunk = os.read(file_descriptor, min(64 * 1024, remaining))
        if not chunk:
            raise RuntimeError("Cargo source file changed while being read")
        digest.update(chunk)
        remaining -= len(chunk)
    if os.read(file_descriptor, 1):
        raise RuntimeError("Cargo source file grew while being read")
    return digest.hexdigest()


def inspect_file(parent_descriptor, name, initial, relative_path, budget):
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    descriptor = os.open(name, flags, dir_fd=parent_descriptor)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or not same_identity(initial, before):
            raise RuntimeError("Cargo source entry changed before file open")
        if before.st_size > MAX_FILE_BYTES:
            raise RuntimeError("Cargo source file exceeds bounded size")
        first_digest = read_digest(descriptor, before.st_size)
        middle = os.fstat(descriptor)
        os.lseek(descriptor, 0, os.SEEK_SET)
        second_digest = read_digest(descriptor, before.st_size)
        after = os.fstat(descriptor)
        final = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
        if (
            not same_identity(before, middle)
            or not same_identity(middle, after)
            or not same_identity(after, final)
            or first_digest != second_digest
        ):
            raise RuntimeError("Cargo source file changed while being read")
        budget["bytes"] += before.st_size
        if budget["bytes"] > MAX_TOTAL_BYTES:
            raise RuntimeError("Cargo source tree exceeds bounded bytes")
        return {
            "kind": "file",
            "path": relative_path,
            "sha256": first_digest,
            "size": before.st_size,
        }
    finally:
        os.close(descriptor)


def inspect_directory(descriptor, relative_path, depth, budget, records):
    if depth > MAX_DEPTH:
        raise RuntimeError("Cargo source tree exceeds bounded depth")
    before = os.fstat(descriptor)
    if not stat.S_ISDIR(before.st_mode):
        raise RuntimeError("Cargo source directory is not a directory")
    names = os.listdir(descriptor)
    names.sort(key=os.fsencode)
    for name in names:
        budget["entries"] += 1
        if budget["entries"] > MAX_ENTRIES:
            raise RuntimeError("Cargo source tree exceeds bounded entries")
        initial = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        child_path = f"{relative_path}/{name}" if relative_path else name
        if stat.S_ISDIR(initial.st_mode):
            flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
            if hasattr(os, "O_CLOEXEC"):
                flags |= os.O_CLOEXEC
            child = os.open(name, flags, dir_fd=descriptor)
            try:
                opened = os.fstat(child)
                if not same_identity(initial, opened):
                    raise RuntimeError("Cargo source directory changed before open")
                records.append({"kind": "directory", "path": child_path})
                inspect_directory(child, child_path, depth + 1, budget, records)
                final = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if not same_identity(opened, final):
                    raise RuntimeError("Cargo source directory changed while being read")
            finally:
                os.close(child)
            continue
        if stat.S_ISREG(initial.st_mode):
            records.append(
                inspect_file(descriptor, name, initial, child_path, budget)
            )
            continue
        raise RuntimeError("Cargo source tree contains unsupported entry type")
    after = os.fstat(descriptor)
    if not same_identity(before, after):
        raise RuntimeError("Cargo source directory changed while being read")


def main():
    if len(sys.argv) != 2:
        raise RuntimeError("usage: inspect-cargo-source-tree.py <root>")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    root = os.open(sys.argv[1], flags)
    try:
        root_identity = os.fstat(root)
        records = []
        inspect_directory(
            root,
            "",
            0,
            {"bytes": 0, "entries": 0},
            records,
        )
        final_identity = os.fstat(root)
        if not same_identity(root_identity, final_identity):
            raise RuntimeError("Cargo source root changed while being read")
        print(
            json.dumps(
                {
                    "records": records,
                    "root": {
                        "dev": str(root_identity.st_dev),
                        "ino": str(root_identity.st_ino),
                        "mode": str(root_identity.st_mode),
                    },
                },
                ensure_ascii=True,
                separators=(",", ":"),
            )
        )
    finally:
        os.close(root)


try:
    main()
except Exception as error:
    print(str(error), file=sys.stderr)
    sys.exit(1)
