#!/usr/bin/env python3
"""B1-0066 writing aid: help_table.py FILE SPEC_FN CONST_NAME TABLE.tsv

Inserts `const CONST_NAME: &[(&str, &str)] = &[...]` (the tool's tooltips) before
`fn SPEC_FN(` in FILE and `with_help(&mut VAR, CONST_NAME);` before the `ToolSpec {`
literal the spec function returns (VAR = the vec named by its `params:` field, made
`mut` if needed). TABLE: one `key<TAB>sentence` per line (# comments allowed); each
sentence must end in . ? ) or …. Run `cargo fmt` afterwards; the controls still
without help: `PLUMSPIRIT_HELP_TOOL=id cargo test -p plumspirit_engine --test
b1_tooltips missing -- --ignored --nocapture`. Standard library only."""
import sys, re, json
path, fn, const, table = sys.argv[1:5]
rows = []
for line in open(table, encoding='utf-8'):
    line = line.rstrip('\n')
    if not line.strip() or line.startswith('#'):
        continue
    k, h = line.split('\t', 1)
    h = h.strip()
    assert h[-1] in '.?)…', (k, h)
    assert '"' not in h or True
    rows.append((k.strip(), h))
keys = [k for k, _ in rows]
assert len(keys) == len(set(keys)), [k for k in keys if keys.count(k) > 1]
s = open(path, encoding='utf-8').read()
i = s.index(f'fn {fn}(')
# start of the item (include a preceding `pub ` / doc comment block)
line_start = s.rindex('\n', 0, i) + 1
j = line_start
# walk back over doc comments / attributes
while True:
    prev = s.rindex('\n', 0, j - 1) + 1 if j > 0 else 0
    seg = s[prev:j].strip()
    if seg.startswith('///') or seg.startswith('#['):
        j = prev
    else:
        break
def lit(h):
    return json.dumps(h, ensure_ascii=False)
body = ''.join(f'    ({lit(k)}, {lit(h)}),\n' for k, h in rows)
decl = f'/// B1-0066: the tooltips of this tool\'s controls (each checked against its kernel).\nconst {const}: &[(&str, &str)] = &[\n{body}];\n\n'
assert f'const {const}:' not in s, 'already applied'
s = s[:j] + decl + s[j:]
# find the fn again and its returned ToolSpec literal
i = s.index(f'fn {fn}(')
body_start = s.index('{', s.index(')', i)) + 1
fn_end = s.find('\n}\n', body_start)
k = s.find('ToolSpec {', body_start)
if k == -1 or k > fn_end:
    # Tools built by a shared helper (`let mut s = spec(…)` … `s`): the help
    # goes on the returned spec's params, before the final `s`.
    tail = s.rfind('\n    s\n', body_start, fn_end + 1)
    if tail == -1 or 'let mut s = ' not in s[body_start:fn_end]:
        # Generators: `let own = vec![…]; super::spec(id, name, summary, own, …)`
        # — the help goes on `own` before the spec call.
        mo = re.search(r'let (mut )?([a-z_][a-z0-9_]*) = vec!\[', s[body_start:fn_end])
        call = re.search(r'\n(\s*)(super::)?spec\(', s[body_start:fn_end])
        if not mo or not call:
            raise SystemExit('no ToolSpec literal, no `let mut s = …; s` and no generator `spec(` in ' + fn)
        var = mo.group(2)
        at = body_start + call.start() + 1
        s = s[:at] + f'{call.group(1)}with_help(&mut {var}, {const});\n' + s[at:]
        if not mo.group(1):
            a = body_start + mo.start()
            s = s[:a] + 'let mut ' + s[a + 4:]
        open(path, 'w', encoding='utf-8').write(s)
        print('applied', len(rows), 'to', fn, 'via', var)
        raise SystemExit(0)
    s = s[:tail + 1] + f'    with_help(&mut s.params, {const});\n' + s[tail + 1:]
    open(path, 'w', encoding='utf-8').write(s)
    print('applied', len(rows), 'to', fn, 'via s.params')
    raise SystemExit(0)
# variable name: the vec passed as `params: X` inside the literal
m = re.search(r'params:\s*([a-z_][a-z0-9_]*)', s[k:k + 4000])
if m:
    var = m.group(1)
elif re.search(r'\bparams,', s[k:k + 4000]):
    var = 'params'
else:
    raise SystemExit('no params field found')
ls = s.rindex('\n', 0, k) + 1
# the params vec must be mutable
body = s[body_start:k]
mm = re.search(r'let\s+' + re.escape(var) + r'\s*(:[^=]+)?=', body)
if mm and 'mut' not in mm.group(0):
    fixed = mm.group(0).replace('let ', 'let mut ', 1)
    s = s[:body_start] + body.replace(mm.group(0), fixed, 1) + s[k:]
    k += len(fixed) - len(mm.group(0))
    ls = s.rindex('\n', 0, k) + 1
indent = re.match(r'\s*', s[ls:]).group(0)
# ToolSpec may be preceded by `return ` or nothing
s = s[:ls] + f'{indent}with_help(&mut {var}, {const});\n' + s[ls:]
open(path, 'w', encoding='utf-8').write(s)
print('applied', len(rows), 'to', fn, 'var', var)
