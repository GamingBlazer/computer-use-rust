"""Keeps the user's clipboard safe while tests that can touch it run (typing pastes through the clipboard).

  python -I clipguard.py save <file>      store every memory-backed clipboard format in <file>
  python -I clipguard.py check <file>     compare the clipboard with <file> (exit 0 same, 1 different)
  python -I clipguard.py restore <file>   put <file> back (only if it differs), then verify (exit 0 same, 1 different)

The snapshot holds whatever the clipboard held, so keep the file private and delete it when the run is over. Formats that
are not plain memory (bitmaps, metafile handles, GDI objects, owner-display) cannot be saved this way and are listed in
the output; text, HTML, rich text, files lists and the like are all memory formats.
"""
import base64
import ctypes
import json
import sys
import time
from ctypes import wintypes

u = ctypes.windll.user32
k = ctypes.windll.kernel32
u.OpenClipboard.argtypes = [wintypes.HWND]
u.EnumClipboardFormats.argtypes = [wintypes.UINT]
u.EnumClipboardFormats.restype = wintypes.UINT
u.GetClipboardData.argtypes = [wintypes.UINT]
u.GetClipboardData.restype = wintypes.HANDLE
u.SetClipboardData.argtypes = [wintypes.UINT, wintypes.HANDLE]
u.SetClipboardData.restype = wintypes.HANDLE
u.GetClipboardFormatNameW.argtypes = [wintypes.UINT, wintypes.LPWSTR, ctypes.c_int]
k.GlobalAlloc.restype = wintypes.HGLOBAL
k.GlobalAlloc.argtypes = [wintypes.UINT, ctypes.c_size_t]
k.GlobalLock.restype = ctypes.c_void_p
k.GlobalLock.argtypes = [wintypes.HGLOBAL]
k.GlobalUnlock.argtypes = [wintypes.HGLOBAL]
k.GlobalSize.restype = ctypes.c_size_t
k.GlobalSize.argtypes = [wintypes.HGLOBAL]

NOT_MEMORY = {2, 3, 9, 14, 0x82, 0x83, 0x8E}  # bitmap, metafile, palette, enhanced metafile, owner-display ...


def open_clip():
    for _ in range(100):
        if u.OpenClipboard(None):
            return True
        time.sleep(0.05)
    return False


def read_all():
    """[(format, bytes)] of every memory format, and the list of formats that could not be read."""
    if not open_clip():
        raise SystemExit("cannot open the clipboard")
    items, skipped = [], []
    try:
        f = 0
        while True:
            f = u.EnumClipboardFormats(f)
            if not f:
                break
            if f in NOT_MEMORY or 0x200 <= f <= 0x3FF:
                skipped.append(f)
                continue
            h = u.GetClipboardData(f)
            if not h:
                skipped.append(f)
                continue
            p = k.GlobalLock(h)
            if not p:
                skipped.append(f)
                continue
            n = k.GlobalSize(h)
            items.append((f, ctypes.string_at(p, n)))
            k.GlobalUnlock(h)
    finally:
        u.CloseClipboard()
    return items, skipped


def write_all(items):
    if not open_clip():
        raise SystemExit("cannot open the clipboard")
    try:
        u.EmptyClipboard()
        for f, data in items:
            h = k.GlobalAlloc(0x42, max(1, len(data)))
            p = k.GlobalLock(h)
            ctypes.memmove(p, data, len(data))
            k.GlobalUnlock(h)
            u.SetClipboardData(f, h)
    finally:
        u.CloseClipboard()


def name_of(f):
    b = ctypes.create_unicode_buffer(128)
    return b.value if u.GetClipboardFormatNameW(f, b, 128) else str(f)


def same(a, b):
    # registered formats keep their numbers within a session, so formats compare by number; order does not matter
    return dict(a) == dict(b)


def main():
    cmd, path = sys.argv[1], sys.argv[2]
    if cmd == "save":
        items, skipped = read_all()
        with open(path, "w") as f:
            json.dump({"items": [[fmt, base64.b64encode(d).decode()] for fmt, d in items], "skipped": skipped}, f)
        print(f"saved {len(items)} format(s) {[fmt for fmt, _ in items]}, skipped {skipped}")
        return 0
    saved = json.load(open(path))
    want = [(fmt, base64.b64decode(d)) for fmt, d in saved["items"]]
    now, _ = read_all()
    if cmd == "check":
        ok = same(want, now)
        print("clipboard identical to the snapshot" if ok else f"clipboard DIFFERS from the snapshot (now formats {[fmt for fmt, _ in now]})")
        return 0 if ok else 1
    if cmd == "restore":
        if same(want, now):
            print("clipboard already identical to the snapshot; nothing to restore")
            return 0
        write_all(want)
        again, _ = read_all()
        ok = same(want, again)
        print("clipboard restored and verified identical" if ok else "clipboard restore could NOT be verified")
        return 0 if ok else 1
    raise SystemExit("usage: clipguard.py save|check|restore <file>")


if __name__ == "__main__":
    sys.exit(main())
