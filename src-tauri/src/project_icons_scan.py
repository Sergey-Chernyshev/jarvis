# Shipped by Jarvis and invoked with python -I -c, never imported from a project.
import base64
import collections
import json
import os
import stat
import sys

MAX_FILE = 128 * 1024
MAX_TOTAL = 768 * 1024
MAX_RAW = 24
MAX_ENTRIES = 6000
MAX_DIRS = 256
MAX_DEPTH = 5
IGNORE = {'node_modules', 'vendor', 'target', 'dist', 'build', 'coverage', '__pycache__'}


def icon_name(name):
    stem, extension = os.path.splitext(name.lower())
    if extension not in ('.png', '.jpg', '.jpeg', '.webp', '.ico', '.svg'):
        return False
    for prefix in ('favicon', 'apple-touch-icon', 'apple-icon', 'icon', 'logo'):
        if stem == prefix:
            return True
        if stem.startswith(prefix) and stem[len(prefix):len(prefix) + 1] in '-_.0123456789':
            return True
    return False


def rank(name):
    if name in ('public', 'static', 'assets', 'app'):
        return 0
    return 1 if name in ('src', 'apps', 'packages') else 2


def scan(root):
    root = os.path.realpath(root)
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
    root_fd = os.open(root, flags | os.O_DIRECTORY)
    queue = collections.deque([(root_fd, '', 0)])
    candidates = []
    visited = entries_seen = total = 0
    truncated = False
    try:
        while queue:
            directory_fd, relative_directory, depth = queue.popleft()
            try:
                if visited >= MAX_DIRS:
                    truncated = True
                    break
                visited += 1
                try:
                    with os.scandir(directory_fd) as iterator:
                        names = []
                        for entry in iterator:
                            names.append(entry.name)
                            if len(names) > MAX_ENTRIES - entries_seen:
                                break
                except OSError:
                    continue
                names.sort(key=lambda name: (rank(name), name))
                for name in names:
                    entries_seen += 1
                    if entries_seen > MAX_ENTRIES:
                        return {'candidates': candidates, 'truncated': True}
                    if name.startswith('.') or name in IGNORE:
                        continue
                    fd = None
                    try:
                        fd = os.open(name, flags, dir_fd=directory_fd)
                        metadata = os.fstat(fd)
                        relative = relative_directory + '/' + name if relative_directory else name
                        if stat.S_ISDIR(metadata.st_mode):
                            if depth < MAX_DEPTH:
                                if visited + len(queue) < MAX_DIRS:
                                    queue.append((fd, relative, depth + 1))
                                    fd = None  # Ownership transferred to the queue.
                                else:
                                    truncated = True
                            continue
                        if not stat.S_ISREG(metadata.st_mode) or not icon_name(name):
                            continue
                        if metadata.st_size == 0 or metadata.st_size > MAX_FILE:
                            continue
                        if len(relative.encode('utf-8', 'replace')) > 1024 or any(ord(c) < 32 or ord(c) == 127 for c in relative):
                            continue
                        with os.fdopen(fd, 'rb') as source:
                            fd = None
                            data = source.read(MAX_FILE + 1)
                        if not data or len(data) > MAX_FILE:
                            continue
                        if total + len(data) > MAX_TOTAL or len(candidates) >= MAX_RAW:
                            return {'candidates': candidates, 'truncated': True}
                        total += len(data)
                        candidates.append({'path': relative, 'data': base64.b64encode(data).decode('ascii')})
                    except OSError:
                        continue
                    finally:
                        if fd is not None:
                            os.close(fd)
            finally:
                os.close(directory_fd)
    finally:
        for fd, _, _ in queue:
            os.close(fd)
    return {'candidates': candidates, 'truncated': truncated}


try:
    result = scan(sys.argv[1])
except (OSError, ValueError, IndexError) as error:
    print(str(error), file=sys.stderr)
    sys.exit(1)
print(json.dumps(result, ensure_ascii=True, separators=(',', ':')))
