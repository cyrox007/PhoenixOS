from pathlib import Path
import textwrap

workflow = Path('.github/workflows/implement-internal-merge.yml').read_text().splitlines()
start = next(i for i, line in enumerate(workflow) if "python3 - <<'PY'" in line) + 1
end = next(i for i, line in enumerate(workflow[start:], start) if line.strip() == 'PY')
script = textwrap.dedent('\n'.join(workflow[start:end]))

marker_start = script.index('hook_anchor =')
marker_end = script.index("hook = r'''", marker_start)
replacement = (
    "hook_marker = '    let requested_blocks ='\n"
    "function_start = s.index(anchor)\n"
    "hook_position = s.index(hook_marker, function_start)\n"
)
script = script[:marker_start] + replacement + script[marker_end:]
script = script.replace(
    's = s.replace(hook_end, hook + hook_end, 1)',
    's = s[:hook_position] + hook + s[hook_position:]',
    1,
)

namespace = {'__name__': '__main__'}
exec(compile(script, 'internal-merge-patch', 'exec'), namespace)
