#!/usr/bin/env python3
"""Extract Phoenix (Discord native Rust Android) UI & bridge schema from base.apk.

Generates both machine-readable JSON (`docs/phoenix-<ver>.json`) and a concise
Markdown lookup reference (`docs/phoenix-<ver>.md`) covering:
  - All ViewKinds, from the static initializer that lists them
  - All prop/event wire keys in wire order, from the static initializer that
    lists them, compared against `crates/ui-mobile/src/props.in`
  - All `hh.jb` host module IDs, method IDs (`Module.method`), and `@event` IDs
  - All `res://ic_mana_*` drawable icons in `base.apk`
  - Known `accessibilityId` patterns referenced in phoenix-unlock

Zero external dependencies (Python stdlib only).
"""

from __future__ import annotations

import argparse
import json
import re
import struct
import zipfile
from collections import defaultdict
from pathlib import Path


def read_uleb128(buf: bytes, off: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while True:
        byte = buf[off]
        off += 1
        result |= (byte & 0x7F) << shift
        if not (byte & 0x80):
            return result, off
        shift += 7


def parse_dex_strings(dex: bytes) -> list[str]:
    s_size, s_off = struct.unpack_from("<II", dex, 56)
    strings: list[str] = []
    for i in range(s_size):
        data_off = struct.unpack_from("<I", dex, s_off + i * 4)[0]
        _, off = read_uleb128(dex, data_off)
        end = dex.index(0, off)
        strings.append(dex[off:end].decode("utf-8", "replace"))
    return strings


# Code units per Dalvik opcode, from the dex bytecode format table.
INSN_UNITS = [1] * 256
for _ops, _units in (
    ((0x02, 0x05, 0x08, 0x13, 0x15, 0x16, 0x19, 0x1A, 0x1C, 0x1F, 0x20, 0x22, 0x23, 0x29, 0xFE, 0xFF), 2),
    ((0x03, 0x06, 0x09, 0x14, 0x17, 0x1B, 0x24, 0x25, 0x26, 0x2A, 0x2B, 0x2C, 0xFC, 0xFD), 3),
    ((0xFA, 0xFB), 4),
    ((0x18,), 5),
    (range(0x2D, 0x3E), 2),
    (range(0x44, 0x6E), 2),
    (range(0x6E, 0x73), 3),
    (range(0x74, 0x79), 3),
    (range(0x90, 0xB0), 2),
    (range(0xD0, 0xE3), 2),
):
    for _op in _ops:
        INSN_UNITS[_op] = _units


def code_strings(dex: bytes, strings: list[str], code_off: int) -> list[str]:
    """Strings loaded by `const-string` in one method body, in code order."""
    units = struct.unpack_from("<I", dex, code_off + 12)[0]
    base = code_off + 16
    out: list[str] = []
    k = 0
    while k < units:
        unit = struct.unpack_from("<H", dex, base + k * 2)[0]
        op = unit & 0xFF
        if unit == 0x0100:
            k += struct.unpack_from("<H", dex, base + k * 2 + 2)[0] * 2 + 4
            continue
        if unit == 0x0200:
            k += struct.unpack_from("<H", dex, base + k * 2 + 2)[0] * 4 + 2
            continue
        if unit == 0x0300:
            width, size = struct.unpack_from("<HI", dex, base + k * 2 + 2)
            k += (width * size + 1) // 2 + 4
            continue
        if op == 0x1A:
            out.append(strings[struct.unpack_from("<H", dex, base + k * 2 + 2)[0]])
        elif op == 0x1B:
            out.append(strings[struct.unpack_from("<I", dex, base + k * 2 + 2)[0]])
        k += INSN_UNITS[op]
    return out


def static_initializers(dex: bytes) -> dict[str, list[str]]:
    """Each class's `<clinit>` strings, so tables are found by their contents
    instead of by obfuscated class names that change every build."""
    strings = parse_dex_strings(dex)
    t_off = struct.unpack_from("<I", dex, 68)[0]
    m_off = struct.unpack_from("<I", dex, 92)[0]
    c_size, c_off = struct.unpack_from("<II", dex, 96)
    out: dict[str, list[str]] = {}
    for ci in range(c_size):
        cidx, *_, cdata_off, _ = struct.unpack_from("<IIIIIIII", dex, c_off + ci * 32)
        if cdata_off == 0:
            continue
        o = cdata_off
        static_fields, o = read_uleb128(dex, o)
        instance_fields, o = read_uleb128(dex, o)
        direct_methods, o = read_uleb128(dex, o)
        _, o = read_uleb128(dex, o)
        for _ in range(static_fields + instance_fields):
            _, o = read_uleb128(dex, o)
            _, o = read_uleb128(dex, o)
        midx = 0
        for _ in range(direct_methods):
            diff, o = read_uleb128(dex, o)
            midx += diff
            _, o = read_uleb128(dex, o)
            code_off, o = read_uleb128(dex, o)
            name = strings[struct.unpack_from("<HHI", dex, m_off + midx * 8)[2]]
            if code_off and name == "<clinit>":
                cls = strings[struct.unpack_from("<I", dex, t_off + cidx * 4)[0]]
                out[cls] = code_strings(dex, strings, code_off)
    return out


def table_holding(inits: dict[str, list[str]], markers: set[str]) -> list[str]:
    """The longest static initializer holding every marker string."""
    found = [strs for strs in inits.values() if markers <= set(strs)]
    return max(found, key=len, default=[])


def extract_accessibility_ids(repo_root: Path) -> list[str]:
    patterns = set()
    scan_dirs = [repo_root / "crates" / "runtime" / "src", repo_root / "plugins"]
    aid_re = re.compile(
        r'"((?:composer\.[a-z0-9._:-]+|message\.[a-z0-9._:-]+|search\.[a-z0-9._:-]+|settings\.[a-z0-9._:-]+|profile-[a-z0-9._:-]+|expression\.[a-z0-9._:-]+|phoenix\.[a-z0-9._:{}-]+|main-tabs|suggestions|composer|send|Record Voice Message))"'
    )
    for base in scan_dirs:
        if not base.exists():
            continue
        for p in base.rglob("*"):
            if p.suffix not in (".rs", ".ts"):
                continue
            txt = p.read_text("utf-8", errors="ignore")
            for m in aid_re.finditer(txt):
                patterns.add(m.group(1))
    return sorted(patterns)


def load_props_in(repo_root: Path) -> list[str]:
    return re.findall(r'"([^"]+)"', (repo_root / "crates" / "ui" / "src" / "props.in").read_text("utf-8"))


WIRE_TYPE_HINTS: dict[str, list[str]] = {
    "float (Value::F64)": [
        "opacity", "scrimOpacity", "width", "height", "minWidth", "maxWidth",
        "minHeight", "maxHeight", "radius", "radiusTopLeft", "radiusTopRight",
        "radiusBottomLeft", "radiusBottomRight", "fontSize", "lineHeight",
        "gap", "gapX", "gapY", "flex", "scale", "scaleX", "scaleY",
        "translateX", "translateY", "rotation", "elevation", "borderWidth",
        "hitSlop",
    ],
    "rect (Value::Rect([l,t,r,b]))": [
        "padding", "margin", "activePadding", "activeMargin",
        "rangeInsets", "scrollIndicatorInsets",
    ],
    "color (Value::I64/I32 ARGB)": [
        "color", "background", "activeBackground", "borderColor", "tint",
        "scrimColor", "navScrimColor", "handleColor", "placeholderColor",
        "focusBorderColor", "minTrackTint", "maxTrackTint", "thumbTint",
        "elevationColor", "bg", "bgBorder",
    ],
    "int (Value::I32)": [
        "fontWeight", "maxLines", "index", "first", "seq", "expandSeq",
    ],
    "bool (Value::Bool)": [
        "hidden", "clip", "selectable", "multiline", "secure", "disabled",
        "passthrough", "announcePolite", "dismissible", "expanded",
        "handleHidden", "underSheets", "lineHeightExact", "includeFontPadding",
    ],
}

OVERLAY_PROPS: dict[str, dict[str, str]] = {
    "banner": {
        "edge": '"top" | "bottom" (top auto-pads status bar + cutout)',
        "passthrough": "bool (false = card intercepts taps, outside passes through)",
        "announce": "string (accessibility announcement)",
        "announcePolite": "bool",
    },
    "sheet": {
        "dismissible": "bool",
        "expanded": "bool",
        "expandSeq": "i32",
        "handleColor": "ARGB color",
        "handleHidden": "bool",
        "handleOverlay": "bool",
        "sheetClip": "bool",
        "sheetMaxHeight": "float",
        "scrimColor": "ARGB color",
        "scrimOpacity": "float",
        "background": "ARGB color",
        "radius / radiusTopLeft": "float",
        "on": '["dismiss"]',
    },
    "popover": {
        "placement": '"top" | "bottom" | "left" | "right" | "auto"',
        "align": '"start" | "center" | "end"',
        "edgeInset": "float",
        "offset": "float",
        "passthrough": "bool",
        "underSheets": "bool",
    },
    "modal": {
        "dismissible": "bool",
        "scrim": "bool",
        "scrimColor": "ARGB color",
        "scrimOpacity": "float",
        "backdropBlur": "float",
    },
}


def build_markdown(schema: dict) -> str:
    lines: list[str] = []
    ver = schema["versionCode"]
    lines.append(f"# Phoenix Android Schema Reference (versionCode `{ver}`)")
    lines.append("")
    lines.append(
        "> Auto-generated from `base.apk` by `scripts/extract-schema.py`. "
        "Use this instead of re-disassembling `classes.dex`."
    )
    lines.append("")

    # ViewKinds
    lines.append(f"## 1. ViewKinds ({len(schema['viewKinds'])})")
    lines.append("")
    lines.append(", ".join(f"`{v}`" for v in schema["viewKinds"]))
    lines.append("")

    # Overlay props
    lines.append("## 2. Native Overlay View Props")
    lines.append("")
    for kind, props in schema["overlayProps"].items():
        lines.append(f"### `{kind}`")
        for k, desc in props.items():
            lines.append(f"- `{k}`: {desc}")
        lines.append("")

    # Wire types
    lines.append("## 3. Wire Type Cheat-Sheet (Avoid Kotlin `ClassCastException`)")
    lines.append("")
    for wtype, names in schema["wireTypeHints"].items():
        lines.append(f"- **{wtype}**: " + ", ".join(f"`{n}`" for n in names))
    lines.append("")

    # Host modules
    lines.append(
        f"## 4. Host Modules & Methods (`hh.jb` — {len(schema['hostModules'])} modules, "
        f"{sum(len(v['methods']) for v in schema['hostModules'].values())} methods)"
    )
    lines.append("")
    lines.append("Call via `phoenix.bridge.invoke(moduleId, methodId, args, body)`.")
    lines.append("")
    lines.append("| Module | ID | Methods (`name: id`) |")
    lines.append("| --- | ---: | --- |")
    for mod, info in sorted(schema["hostModules"].items(), key=lambda kv: kv[1]["id"]):
        meths = ", ".join(
            f"`{m}: {mid}`" for m, mid in sorted(info["methods"].items(), key=lambda x: x[1])
        )
        lines.append(f"| `{mod}` | `{info['id']}` | {meths or '—'} |")
    lines.append("")

    # Host events
    lines.append(f"## 5. Host Dispatch Events (`@event` — {len(schema['hostEvents'])})")
    lines.append("")
    ev_list = ", ".join(
        f"`{k}: {v}`" for k, v in sorted(schema["hostEvents"].items(), key=lambda x: x[1])
    )
    lines.append(ev_list)
    lines.append("")

    # Accessibility IDs
    lines.append(f"## 6. Known `accessibilityId` Patterns ({len(schema['accessibilityIds'])})")
    lines.append("")
    lines.append(", ".join(f"`{a}`" for a in schema["accessibilityIds"]))
    lines.append("")

    # Icons summary
    icons = schema["manaIcons"]
    lines.append(f"## 7. Mana Icons (`res://ic_mana_*` — {len(icons)} total)")
    lines.append("")
    lines.append(
        "Full list is in `phoenix-" + str(ver) + ".json` under `manaIcons`. "
        "Commonly used icons:"
    )
    lines.append("")
    common = [
        ic for ic in icons
        if any(
            k in ic
            for k in (
                "puzzle", "x_small", "checkmark", "angle_brackets",
                "image_file", "eye", "copy", "trash", "pencil",
                "arrow_small", "circle_information", "bell", "sparkle",
            )
        )
    ][:40]
    lines.append(", ".join(f"`res://{ic}`" for ic in common))
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    repo_root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "apk",
        nargs="?",
        default=str(repo_root.parent / "arm64" / "dl" / "base.apk"),
        help="Path to Discord Phoenix base.apk",
    )
    parser.add_argument(
        "--version-code",
        default="356207",
        help="Discord versionCode label for output filenames",
    )
    parser.add_argument(
        "--out-dir",
        default=str(repo_root / "docs"),
        help="Output directory for JSON and Markdown reference files",
    )
    args = parser.parse_args()

    apk_path = Path(args.apk)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    with zipfile.ZipFile(apk_path) as zf:
        dex = zf.read("classes.dex")
        inits: dict[str, list[str]] = {}
        for name in sorted(n for n in zf.namelist() if re.fullmatch(r"classes\d*\.dex", n)):
            inits.update(static_initializers(zf.read(name)))
        mana_icons = sorted(
            {
                Path(name).stem
                for name in zf.namelist()
                if "/ic_mana_" in name and name.startswith("res/drawable")
            }
        )

    strings = parse_dex_strings(dex)
    view_kinds = [
        s
        for s in table_holding(inits, {"view", "text", "sheet", "banner"})
        if re.fullmatch(r"[a-z][A-Za-z0-9_]*", s)
    ]
    props_table = table_holding(inits, {"absolute", "accessibilityActions"})
    props_in = load_props_in(repo_root)
    shifted = next((i for i, (a, b) in enumerate(zip(props_in, props_table)) if a != b), None)
    if shifted is not None:
        print(f"props.in index {shifted} is {props_in[shifted]!r}, the APK has {props_table[shifted]!r}: regenerate props.in")
    elif len(props_table) != len(props_in):
        print(f"the APK appends {len(props_table) - len(props_in)} props after props.in's {len(props_in)}")

    # Parse hh.jb modules, methods, and @events
    mod_re = re.compile(r"^([A-Z][A-Za-z0-9_]+) (\d+)$")
    meth_re = re.compile(r"^([A-Z][A-Za-z0-9_]+)\.([A-Za-z0-9_]+) (\d+)$")
    ev_re = re.compile(r"^(@[A-Za-z0-9_]+) (\d+)$")

    modules: dict[str, dict] = {}
    methods_by_mod: dict[str, dict[str, int]] = defaultdict(dict)
    events: dict[str, int] = {}

    for s in strings:
        if m := meth_re.match(s):
            methods_by_mod[m.group(1)][m.group(2)] = int(m.group(3))
        elif m := ev_re.match(s):
            events[m.group(1)] = int(m.group(2))

    # Filter module names to real modules (either have methods or known standalone modules)
    non_modules = {"Pixel", "Top"}
    for s in strings:
        if m := mod_re.match(s):
            name, mid = m.group(1), int(m.group(2))
            if name in non_modules:
                continue
            if name in methods_by_mod or mid in (
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
                19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34,
                35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
                51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
            ):
                modules[name] = {"id": mid, "methods": dict(sorted(methods_by_mod.get(name, {}).items()))}

    for mod, meths in methods_by_mod.items():
        if mod not in modules:
            modules[mod] = {"id": -1, "methods": dict(sorted(meths.items()))}

    aids = extract_accessibility_ids(repo_root)

    schema = {
        "versionCode": args.version_code,
        "viewKinds": view_kinds,
        "propCount": len(props_table),
        "props": props_table,
        "wireTypeHints": WIRE_TYPE_HINTS,
        "overlayProps": OVERLAY_PROPS,
        "hostModules": dict(sorted(modules.items(), key=lambda kv: kv[1]["id"])),
        "hostEvents": dict(sorted(events.items(), key=lambda kv: kv[1])),
        "manaIcons": mana_icons,
        "accessibilityIds": aids,
    }

    json_path = out_dir / f"phoenix-{args.version_code}.json"
    md_path = out_dir / f"phoenix-{args.version_code}.md"
    json_path.write_text(json.dumps(schema, indent=2) + "\n", encoding="utf-8")
    md_path.write_text(build_markdown(schema), encoding="utf-8")

    print(
        f"Wrote {json_path} & {md_path}: "
        f"{len(view_kinds)} viewKinds, {len(props_table)} props, "
        f"{len(modules)} hostModules ({sum(len(m['methods']) for m in modules.values())} methods), "
        f"{len(events)} @events, {len(mana_icons)} ic_mana icons, {len(aids)} accessibilityIds"
    )


if __name__ == "__main__":
    main()
