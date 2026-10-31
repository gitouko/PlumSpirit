#!/usr/bin/env python3
"""B1-0067: American English in user-facing string literals.

us_english.py scan  [paths...]   list British spellings in string literals
us_english.py fix   [paths...]   rewrite user-facing literals in place

Only the *contents of string literals* change; comments, identifiers and
literals that look like keys / ids (lowercase, no spaces: settings keys,
parameter ids, Spiderweb state keys, file names) are kept, so stored files,
presets and keybind names stay compatible. Owner-chosen proper names
(the Tumours tool) are kept.
"""
import os, re, sys

# Case-preserving replacements, longest first.
WORDS = [
    ("centring", "centering"), ("centred", "centered"), ("centres", "centers"), ("centre", "center"),
    ("colour", "color"), ("neighbour", "neighbor"), ("behaviour", "behavior"),
    ("analysing", "analyzing"), ("analysed", "analyzed"), ("analyse", "analyze"),
    ("licences", "licenses"), ("licence", "license"),
    ("travelling", "traveling"), ("travelled", "traveled"),
    ("cancelling", "canceling"), ("cancelled", "canceled"),
    ("labelling", "labeling"), ("labelled", "labeled"),
    ("modelling", "modeling"), ("modelled", "modeled"),
    ("greyed", "grayed"), ("greyish", "grayish"), ("greys", "grays"), ("grey", "gray"),
    ("favourite", "favorite"), ("honour", "honor"),
    ("catalogue", "catalog"), ("dialogue", "dialog"),
]
STEMS = ("normal|quant|optim|custom|initial|recogn|synchron|visual|organ|random|summar|"
         "categor|priorit|standard|equal|util|harmon|serial|maxim|minim|real")
IZE = re.compile(r"\b(" + STEMS + r")is(e|ed|es|ing|ation|ations|er|ers)\b", re.I)
# Words that may follow a prefix (multicolour, epicentre, misbehaviour).
INNER = ("centring", "centred", "centres", "centre", "colour", "neighbour", "behaviour", "favourite")
WORD_RX = re.compile(
    r"((?:" + "|".join(INNER) + r")|(?<![A-Za-z])(?:" + "|".join(w for w, _ in WORDS if w not in INNER) + r"))",
    re.I,
)
US = {w: u for w, u in WORDS}
KEY_RX = re.compile(r"^[a-z0-9_.:/\-]*$")


def case_like(src, dst):
    if src.isupper():
        return dst.upper()
    if src[0].isupper():
        return dst[0].upper() + dst[1:]
    return dst


def americanize(text):
    out = WORD_RX.sub(lambda m: case_like(m.group(1), US[m.group(1).lower()]), text)
    out = IZE.sub(lambda m: m.group(1) + ("IZ" if m.group(0)[len(m.group(1)):][:2].isupper() else "iz") + m.group(2), out)
    return out


def user_facing(lit):
    """A literal shown to people: not a bare lowercase key / id."""
    return not KEY_RX.match(lit)


def literals(src):
    """(start, end) of the contents of every plain string literal, skipping
    comments, char literals and raw strings."""
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
        elif src.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
        elif c == "r" and re.match(r'r#*"', src[i:i + 8]) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            m = re.match(r'r(#*)"', src[i:])
            close = '"' + m.group(1)
            j = src.find(close, i + len(m.group(0)))
            i = n if j < 0 else j + len(close)
        elif c == "b" and src.startswith('b"', i) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            j = i + 2
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            i = j + 1
        elif c == "'":
            m = re.match(r"'(\\.|\\u\{[0-9a-fA-F]+\}|[^\\'])'", src[i:])
            i += len(m.group(0)) if m else 1
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            yield (i + 1, j)
            i = j + 1
        else:
            i += 1


KEEP = ("Tumours", "tumours", "TUMOURS", "Tumour", "tumour")


def process(path, fix):
    src = open(path, encoding="utf-8").read()
    out, last, hits = [], 0, []
    for a, b in literals(src):
        lit = src[a:b]
        new = americanize(lit)
        if new == lit:
            continue
        if not user_facing(lit) or any(k in lit for k in KEEP if k.lower() in lit.lower() and k in lit) and americanize(lit.replace("Tumour", "\0")).replace("\0", "Tumour") == lit:
            hits.append(("KEEP", src.count("\n", 0, a) + 1, lit))
            continue
        hits.append(("FIX", src.count("\n", 0, a) + 1, lit))
        out.append(src[last:a])
        out.append(new)
        last = b
    if fix and out:
        out.append(src[last:])
        open(path, "w", encoding="utf-8").write("".join(out))
    return hits


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "scan"
    roots = sys.argv[2:] or ["crates/plumspirit/src", "crates/plumspirit_engine/src"]
    fixes = keeps = 0
    for root in roots:
        files = [root] if root.endswith(".rs") else [os.path.join(d, f) for d, _, fs in os.walk(root) for f in fs if f.endswith(".rs")]
        for p in sorted(files):
            if p.endswith("us_english_audit.rs"):
                continue  # the guard's own test inputs are British on purpose
            for kind, line, lit in process(p, mode == "fix"):
                if kind == "FIX":
                    fixes += 1
                else:
                    keeps += 1
                if mode == "scan" or kind == "KEEP":
                    print(f"{kind} {p}:{line}: {lit[:100]}")
    print(f"{fixes} literal(s) {'changed' if mode == 'fix' else 'to change'}, {keeps} kept as keys / names", file=sys.stderr)


if __name__ == "__main__":
    main()
