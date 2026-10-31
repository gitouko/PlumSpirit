"""Development-only reference oracle for PlumSpirit's port of Spiderweb's formula language
(files/mathexpr.py `formula`, MIT, Kanade Tachibana) and of CPython's math.hypot.

Writes crates/plumspirit_engine/tests/fixtures/spiderweb_formula_golden.json: hand-made and seeded
random formulas (valid and invalid), each read with formula(text, named, var) and, when it reads,
worked out at several positions with fixed numbers for its names — exactly what the ORIGINAL code
gives (a float, inf / nan included, or null where it raises).

Usage: python3 formula_oracle.py /path/to/Spiderweb   (Python >= 3.12)
"""
import json
import math
import os
import random
import sys

sys.path.insert(0, os.path.join(sys.argv[1], "scripts"))
from files.mathexpr import formula  # noqa: E402

R = random.Random(8031)
XS = [0.0, 0.25, 0.5, 1.0, -1.5, 3.0, 2.5, -0.0, 1e-300, 1e300, 0.1, 7.0]
VALUES = {"a": 2.0, "b": -0.5, "height": 4.0, "steep": 8.0, "n": 3.0, "t": 0.75, "x": 0.4, "big": 1e308}

HAND = [
    "x", "x^2", "x**2", "-x**2", "(-x)**2", "2**-1", "2**-x", "x**y" if False else "x**a", "2**3**2",
    "-2**2", "+-x", "--x", "- - x", "7 // 2", "-7 // 2", "7 // -2", "7 % 3", "-7 % 3", "7 % -3",
    "-7.5 % 2", "7.5 // -2", "x // 0.3", "x % 0.3", "1/3", "10/4", "2**53 + 1", "(2**53 + 1) / 3",
    "pow(2, 100) / 3", "pow(10, 400)", "pow(10, 400) // pow(10, 399)", "pow(10, 400) / pow(10, 399)",
    "pow(2, 1023) * 2.0", "pow(2, 1024) * 1.0", "pow(2, 1024) / pow(2, 2)", "1 / pow(2, 1080)",
    "3 / pow(2, 1075)", "pow(3, 4, 5)", "pow(3, -1, 7)", "pow(-3, 3, -7)", "pow(2, 3, 0)", "pow(4, -1, 8)",
    "pow(2.0, 3, 5)", "pow(0, -1)", "0 ** -1", "0.0 ** -1", "0 ** 0", "x ** 0", "(-8) ** (1/3)",
    "abs((-8) ** (1/3))", "abs((-1) ** 0.5)", "abs((-1) ** 0.5 * 2)", "abs(((-1) ** 0.5) ** 2)",
    "abs(((-1) ** 0.5) ** -3)", "abs(((-2) ** 0.5) ** 2.5)", "abs((-1) ** 0.5 / (1 + (-1) ** 0.5))",
    "abs(1/(((-1)**0.5)*1e308*1e308))", "abs((-1) ** 0.5 + 1)", "abs((-1) ** 0.5 - x)",
    "abs(-x ** 0.5)", "abs((-x) ** 0.5)", "((-1) ** 0.5) ** 0", "abs(((-1)**0.5) ** 101)",
    "abs(((-1)**0.5) ** 100.5)", "abs(((-1)**0.5)**((-1)**0.5))", "abs(0 * (-1) ** 0.5) ** -1",
    "(-1) ** 0.5 // 2", "abs((-1)**0.5) > 1" if False else "abs((-1) ** 0.5) * 3",
    "round(2.5)", "round(3.5)", "round(-2.5)", "round(2.675, 2)", "round(0.125, 2)", "round(x, 1)",
    "round(1250, -2)", "round(1350, -2)", "round(-1250, -2)", "round(15, -1)", "round(x, -1)",
    "round(12345.678, -2)", "round(1e300, -300)", "round(1.7976931348623157e308, -308)",
    "round(x, 400)", "round(x, -400)", "round(5, -400)", "round(x, 2.0)", "round(True, 1)",
    "round(0.5)", "round(1e300)", "round(x)", "round()", "round(1, 2, 3)", "floor(x)", "ceil(x)",
    "floor(-0.5)", "ceil(-0.5)", "floor(1e300) / 1e299", "floor(pow(10, 30))", "x - floor(x + 0.5)",
    "max(1, 2.0, 2)", "max(2, 2.0)", "max(2.0, 2)", "min(1, True)", "max(x, -x)", "min(x)", "max()",
    "max(pow(2, 60) + 1, 2.0 ** 60)", "min(pow(2, 60) + 1, 2.0 ** 60)", "max(0, sqrt(-1))",
    "max(1e308 * 10 - 1e308 * 10, 1)", "max(1, 1e308 * 10 - 1e308 * 10)", "min(3, x, 2)",
    "max(-1, min(1, steep * sin(x * 2 * pi)))", "max((-1) ** 0.5, 1)",
    "log(x)", "log(0)", "log(-1)", "log(8, 2)", "log(8, 1)", "log(pow(10, 400))", "log10(pow(10, 400))",
    "log2(pow(2, 2000))", "log(pow(10, 400), 10)", "log10(1000)", "log2(1024)", "ln(e)", "log(e**2)",
    "log(0.0)", "log(-0.0)", "log(1e308 * 10)", "log(1e308 * 10 - 1e308 * 10)", "log10(-5)",
    "log2(0)", "log(True)", "log(x, 0)", "log(2, 1.0)", "log(1, 2, 3)", "sqrt(x)", "sqrt(-x)",
    "sqrt(-0.0)", "exp(x)", "exp(1000)", "exp(-1000)", "exp(1e308 * 10)", "sinh(1000)", "cosh(-1000)",
    "tanh(1000)", "sin(1e308 * 10)", "cos(x * pi)", "tan(pi / 2)", "asin(2)", "acos(-1)", "atan(1e308 * 10)",
    "asin(x)", "acos(x)", "sin(pow(10, 400))", "sin()", "sin(1, 2)", "abs(-pow(10, 400)) // 7",
    "abs(-2)", "abs(-x)", "abs()", "pow(x)", "pow(2, 0.5)", "pow(-2, 0.5)", "pow(x, 70)",
    "x ** 64", "x ** 65", "x ** -64.5", "2 ** 64", "2 ** 65", "2 ** 64.0001", "(-2) ** 63",
    "x ** (60 + 5)", "2 ** (1 + (-1) ** 0.5)", "1e308 * 10", "1e308 * 10 - 1e308 * 10", "-1e308 * 10",
    "1 / x", "x / 0", "x // 0", "x % 0", "0 / 0", "1 // 0", "1 % 0", "0x1F", "0X1f + 0o17 + 0b101",
    "0x_1f", "0b1_0", "1_000_000 + x", "1_000.000_1", "1e1_0", ".5", "5.", "5.e-1", "1E5", "01.5",
    "00", "0_0", "09.5", "0e0", "007e1", "0123", "1__0", "1_", "0x", "0b2", "0o8", "1e", "1e+", ".e1",
    "1.5.5", "1j", "2J", "1.5j", "x + 1j", "2x", "2 x", "2pi", "x2", "_1", "_", "__", "e", "pi", "pie",
    "True", "False", "True + True", "True / 2", "-True", "None", "x if x else 1", "x and 1",
    "not x", "x < 1", "x == 1", "x[0]", "x.real", "(x, 1)", "(x,)", "()", "[x]", "{x}", "'x'",
    "lambda: x", "x(2)", "(sin)(x)", "sin(x)(2)", "sin(x,)", "sin(,)", "sin(x,,)", "min(1, 2,)",
    "sin(*x)", "sin(x=1)", "foo(x)", "sin", "x + sin", "x × 2", "x^^2", "x*^2", "x***2", "x+++1",
    "x # a comment", "(x # a comment\n + 1)", "x\n+ 1", "(x\n+ 1)", "x \\\n + 1", "x \\ + 1",
    "(x\r\n+1)", "\tx\t+\t1", "x\x0c+1", "x\x0b+1", "x +1", "  x + 1  ", "\n x \n", "x\n# c",
    "x;1", "x:=1", "x @ 2", "~x", "x << 1", "x & 1", "x | 1", "x != 1", "x**=2", "x*=2", "x+=1",
    "height * sin(x * 2 * pi)", "height * 2 / pi * asin(sin(x * 2 * pi))", "height * abs(sin(x * pi))",
    "height * max(-1, min(1, steep * sin(x * 2 * pi)))", "height * 2 * (x - floor(x + 0.5))",
    "height * (1 - abs(2 * x - 1))", "height * (1 - abs(2 * x - 1))^sharp",
    "height * sqrt(max(0, 1 - (2 * x - 1)^2))", "height * min(steps, floor((1 - abs(2 * x - 1)) * (steps + 1))) / steps",
    "height * sin(x * pi) * sin(x * waves * 2 * pi)", "height * (1 - x) * sin(x * waves * 2 * pi)",
    "height * (1 - cos(x * 2 * pi)) / 2", "curl * sin(x * 2 * pi) / (2 * pi)", "a * x + b",
    "big * 10", "big * big - big * big", "x ** big", "big ** 0.5", "höhe * x", "x²", "ｘ + 1",
    "(" * 200 + "x" + ")" * 200, "(" * 201 + "x" + ")" * 201, "-" * 300 + "x", "1" + "+1" * 500,
    "x" + "*x" * 300, "sin(" * 100 + "x" + ")" * 100, "x ** 0.5 ** 2", "(-x) ** 0.5", "(-0.5) ** x",
    "(-2) ** x", "x ** -1", "(-x) ** -2", "0 ** x", "1 ** big", "(-1) ** big", "(-1.0) ** (big + 0.5)",
]


def rnum():
    r = R.random()
    if r < 0.3:
        return str(R.randint(0, 20))
    if r < 0.55:
        return repr(round(R.uniform(-5, 5), R.randint(0, 4))).lstrip("-")
    if r < 0.6:
        return R.choice(["0x1F", "0b101", "0o7", "1_000", "2.5e-3", "1e2", ".5", "3."])
    if r < 0.65:
        return str(R.randint(10 ** 15, 10 ** 25))
    return R.choice(["x", "x", "x", "pi", "e", "a", "b", "True", "n"])


FN1 = ["sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "sqrt", "exp", "log", "ln",
       "log10", "log2", "abs", "floor", "ceil", "round"]


def rexpr(d=0):
    r = R.random()
    if d > 4 or r < 0.25:
        return rnum()
    if r < 0.6:
        op = R.choice(["+", "-", "*", "/", "//", "%", "**", "^", "*", "+"])
        a, b = rexpr(d + 1), rexpr(d + 1)
        if op in ("**", "^") and R.random() < 0.7:
            b = R.choice(["2", "3", "0.5", "-1", "x", "1/3", "-2", "a"])
        return f"{a} {op} {b}" if R.random() < 0.5 else f"({a}){op}({b})"
    if r < 0.7:
        return R.choice(["-", "+", "-"]) + rexpr(d + 1)
    if r < 0.85:
        return f"{R.choice(FN1)}({rexpr(d + 1)})"
    f = R.choice(["min", "max", "round", "pow", "log"])
    n = R.choice([2, 2, 3]) if f in ("min", "max") else 2
    args = [rexpr(d + 1) for _ in range(n)]
    if f == "pow":
        args[1] = R.choice(["2", "3", "-1", "0.5", "x"])
    if f == "round":
        args[1] = R.choice(["0", "1", "2", "-1", "3"])
    return f"{f}({', '.join(args)})"


def mangle(t):
    """A random small change (most give text that can't be read)."""
    i = R.randrange(len(t) + 1)
    return t[:i] + R.choice(["(", ")", ",", "*", "x", "1", " ", "_", ".", "e", "j", "#", "\n", "[", "'"]) + t[i:]


texts = [(t, "x") for t in HAND]
texts += [("16 * sin(t * 2 * pi)^3", "t"), ("t * cos(t * turns * 2 * pi)", "t"), ("x * t", "t"),
          ("t^steep / (t^steep + (1 - t)^steep)", "t"), ("ln(1 + steep * t) / ln(1 + steep)", "t")]
for _ in range(1500):
    t = rexpr()
    texts.append((t, "x"))
    if R.random() < 0.15:
        texts.append((mangle(t), "x"))


def num(v):
    return v  # json writes NaN / Infinity (PlumSpirit's reader takes them)


out = []
for text, var in texts:
    for named in (True, False):
        if not named and R.random() < 0.8 and text not in HAND:
            continue
        case = {"text": text, "var": var, "named": named}
        try:
            fn = formula(text, named=named, var=var)
        except ValueError:
            case["error"] = True
            out.append(case)
            continue
        case["names"] = list(fn.names)
        vals = {n: VALUES.get(n, 1.5) for n in fn.names}
        case["vars"] = [[n, vals[n]] for n in fn.names]
        res = []
        for x in XS:
            try:
                v = fn(x, vals)
            except (ValueError, ArithmeticError, TypeError, KeyError):
                v = None
            res.append([x, v])
        case["at"] = res
        out.append(case)

R2 = random.Random(77)
hyp = []
for _ in range(4000):
    s = R2.random()
    if s < 0.6:
        x, y = R2.uniform(-100, 100) * 10 ** R2.uniform(-6, 6), R2.uniform(-100, 100) * 10 ** R2.uniform(-6, 6)
    elif s < 0.8:
        x, y = R2.uniform(-1, 1) * 1e-310, R2.uniform(-1, 1) * 10 ** R2.uniform(-320, -300)
    elif s < 0.9:
        x, y = R2.uniform(-1, 1) * 1e308, R2.uniform(-1, 1) * 1e308
    else:
        x, y = float(R2.randint(-50, 50)), float(R2.randint(-50, 50))
    hyp.append([x, y, math.hypot(x, y)])

here = os.path.dirname(os.path.abspath(__file__))
fx = os.path.join(here, "..", "..", "..", "crates", "plumspirit_engine", "tests", "fixtures")
json.dump({"formulas": out, "hypot": hyp}, open(os.path.join(fx, "spiderweb_formula_golden.json"), "w"),
          separators=(",", ":"))
print(len(out), "formula cases,", sum("error" in c for c in out), "refused;", len(hyp), "hypot")
