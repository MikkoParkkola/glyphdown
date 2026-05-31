#!/usr/bin/env python3
"""
UltraCoS skill-level filter (SessionStart hook).
Reads active_level flag, filters SKILL.md blocks by level marker.
Emits filtered content as additionalContext.
"""

import os
import json
import sys
from pathlib import Path


FLAG_FILENAME = "active-level"


def set_active_level(lvl: str) -> None:
    """Atomically write level flag to ~/.ultracos/active-level."""
    home = Path.home()
    ultracos_dir = home / ".ultracos"
    ultracos_dir.mkdir(parents=True, exist_ok=True)

    tmp_file = ultracos_dir / (FLAG_FILENAME + ".tmp")
    flag_file = ultracos_dir / FLAG_FILENAME

    tmp_file.write_text(lvl.strip())
    os.replace(str(tmp_file), str(flag_file))


def get_active_level() -> str:
    """Read active-level, default 'full' if missing."""
    home = Path.home()
    flag_file = home / ".ultracos" / FLAG_FILENAME

    if flag_file.exists():
        return flag_file.read_text().strip()
    return "full"


# Levels ordered by escalating compression. Each level inherits rows
# from all lighter levels (lite ⊂ full ⊂ ultra). 'off' suppresses
# injection entirely.
LEVEL_ORDER = ["lite", "full", "ultra"]


def _level_includes(active: str, row_level: str) -> bool:
    """Row visible if row_level is <= active in LEVEL_ORDER."""
    if active not in LEVEL_ORDER or row_level not in LEVEL_ORDER:
        return active == row_level
    return LEVEL_ORDER.index(row_level) <= LEVEL_ORDER.index(active)


def filter_register_variants_table(content: str, active_level: str) -> str:
    """
    Filter the single-source register-variants table by level.

    Table delimited by:
      <!-- register-variants:start -->
      | header | level | ... |
      |---|---|---|
      | row content | lite | ... |
      | row content | full | ... |
      | row content | ultra | ... |
      <!-- register-variants:end -->

    Header + separator are always kept. Body rows are kept iff the row's
    `level` column is included by the active level (cumulative inheritance).
    """
    start_marker = "<!-- register-variants:start -->"
    end_marker = "<!-- register-variants:end -->"

    if start_marker not in content or end_marker not in content:
        return content

    before, rest = content.split(start_marker, 1)
    table_block, after = rest.split(end_marker, 1)

    lines = table_block.split("\n")
    header_idx = None
    sep_idx = None
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("|") and header_idx is None:
            header_idx = i
        elif header_idx is not None and stripped.startswith("|") and set(stripped.replace("|", "").replace(" ", "")) <= {"-", ":"}:
            sep_idx = i
            break

    if header_idx is None or sep_idx is None:
        # malformed table — pass through unchanged
        return before + start_marker + table_block + end_marker + after

    # Identify the level column index from the header row.
    header_cells = [c.strip().lower() for c in lines[header_idx].strip().strip("|").split("|")]
    try:
        level_col = header_cells.index("level")
    except ValueError:
        return before + start_marker + table_block + end_marker + after

    kept = lines[: sep_idx + 1]
    for line in lines[sep_idx + 1:]:
        stripped = line.strip()
        if not stripped.startswith("|"):
            kept.append(line)
            continue
        cells = [c.strip() for c in stripped.strip("|").split("|")]
        if len(cells) <= level_col:
            kept.append(line)
            continue
        row_level = cells[level_col].lower()
        if _level_includes(active_level, row_level):
            kept.append(line)

    return before + start_marker + "\n".join(kept) + end_marker + after


def filter_skill_md(content: str, active_level: str) -> str:
    """
    Filter SKILL.md by level markers.

    Level blocks delimited by:
      <!-- level:lite -->
      content
      <!-- level:end -->

      <!-- level:full -->
      content
      <!-- level:end -->

    Content outside markers is always included.
    Only blocks matching active_level are included.
    """
    if active_level == "off":
        return ""

    lines = content.split("\n")
    result = []
    i = 0
    current_level = None
    skip_until_end = False

    while i < len(lines):
        line = lines[i]

        # Check for level marker start
        if "<!-- level:" in line and "-->" in line:
            if skip_until_end and "<!-- level:end -->" in line:
                # End of skipped block
                skip_until_end = False
                current_level = None
                i += 1
                continue

            # Extract level from marker
            if "<!-- level:" in line:
                start = line.find("<!-- level:")
                end = line.find(" -->", start)
                if end > start:
                    marker_text = line[start + 11:end].strip()
                    if marker_text == "end":
                        # End of current block
                        current_level = None
                        skip_until_end = False
                    else:
                        # Start of level block
                        current_level = marker_text
                        skip_until_end = (marker_text != active_level)
                i += 1
                continue

        # Regular line: include if not in skipped block
        if not skip_until_end:
            result.append(line)

        i += 1

    return "\n".join(result)


def main() -> int:
    try:
        active_level = get_active_level()

        # If mode is "off", skip injection
        if active_level == "off":
            print(json.dumps({"continue": True}))
            return 0

        # Find SKILL.md relative to this hook's plugin root
        # CLAUDE_PLUGIN_ROOT env var set by harness
        plugin_root = os.environ.get("CLAUDE_PLUGIN_ROOT", "")
        if not plugin_root:
            # Fallback: assume we're in ultracos/hooks/SessionStart
            plugin_root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))

        skill_path = os.path.join(plugin_root, "ultracos", "skills", "ultracos-mode", "SKILL.md")

        if not os.path.exists(skill_path):
            print(json.dumps({"continue": True}))
            return 0

        with open(skill_path, "r") as f:
            content = f.read()

        filtered = filter_skill_md(content, active_level)
        filtered = filter_register_variants_table(filtered, active_level)

        print(json.dumps({
            "continue": True,
            "additionalContext": filtered
        }))
        return 0

    except Exception as e:
        # Fail open: always return continue:true on exception
        print(json.dumps({"continue": True}))
        return 0


if __name__ == "__main__":
    sys.exit(main())
