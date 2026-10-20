#!/usr/bin/env python3
"""apple/scripts/strings-lint.py —— 本地化检查：找出还没本地化的中文字面量，以及字符串目录里缺失 / 多余的键。

规则（docs/design.md 5.5）：
- 代码里的文案字面量是简体原文，同时就是 String Catalog 的键。
- SwiftUI 的 Text / Button / Toggle 等拿字面量时自动当键；AppKit 侧（NSMenu、NSAlert、stringValue…）必须包 String(localized:)。
- App Intents 的 title / IntentDescription / @Parameter(title:) / DisplayRepresentation 本身就是 LocalizedStringResource，也算已本地化。
- 插值 \\(x) 在键里是 %@ / %lld / %llu，脚本按通配匹配。

用法：python3 apple/scripts/strings-lint.py   （退出码 1 = 有问题）
"""
import json, pathlib, re, sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
# target → (目录（含它的 Localizable.xcstrings）, 另外编进这个 target 的共享目录)
TARGETS = {"Piesi": (ROOT / "Piesi", [ROOT / "Shared"]), "PiesiQuickLook": (ROOT / "PiesiQuickLook", [ROOT / "Shared"])}
CJK = re.compile(r"[　-〿一-鿿＀-￯…]")

def literals(code: str):
    """逐字符扫一行 Swift，产出 (起点, 字面量内容)。插值 \\(…) 原样保留，里面套的字符串不会把外层截断。"""
    i, n = 0, len(code)
    while i < n:
        if code[i] != '"':
            i += 1
            continue
        start, i, buf = i, i + 1, []
        while i < n and code[i] != '"':
            if code[i] == "\\" and i + 1 < n and code[i + 1] == "(":
                depth, j = 1, i + 2
                while j < n and depth:
                    if code[j] == '"':  # 插值里的字符串整段跳过
                        j += 1
                        while j < n and code[j] != '"':
                            j += 2 if code[j] == "\\" else 1
                    elif code[j] == "(":
                        depth += 1
                    elif code[j] == ")":
                        depth -= 1
                    j += 1
                buf.append(code[i:j])
                i = j
            elif code[i] == "\\":
                buf.append(code[i:i + 2])
                i += 2
            else:
                buf.append(code[i])
                i += 1
        yield start, "".join(buf)
        i += 1
# 这些调用把字面量当 LocalizedStringKey，不用再包
SWIFTUI_AUTO = re.compile(r"(?:\b(?:Text|Button|Toggle|Section|Picker|Label|TextField|SecureField|DisclosureGroup|ProgressView|ContentUnavailableView|Menu|Link|NavigationLink)\s*\(|\.(?:help|navigationTitle|confirmationDialog|alert)\s*\(|LocalizedStringResource\s*=|LocalizedStringResource\s*\(|IntentDescription\s*\(|@Parameter\(title:|TypeDisplayRepresentation\(name:|DisplayRepresentation\(title:|shortTitle:)\s*$")
# 行尾注释写 `strings-lint: ignore` 的行不查（AppShortcuts 短语在 AppShortcuts.xcstrings 里本地化）
IGNORE_MARK = "strings-lint: ignore"
WRAPPED = re.compile(r"String\(localized:\s*$")
# 不是给用户看的
IGNORE = re.compile(r"fatalError\(|Logger\(|logger\.|Log\.\w+\.|privacy:")

def key_pattern(literal: str) -> re.Pattern:
    """字面量 → 能匹配目录键的正则：插值变成占位通配。"""
    parts, i, cur = [], 0, []
    while i < len(literal):
        if literal.startswith("\\(", i):
            depth, j = 1, i + 2
            while j < len(literal) and depth:
                depth += {"(": 1, ")": -1}.get(literal[j], 0)
                j += 1
            parts.append("".join(cur)); cur = []; i = j
        else:
            cur.append(literal[i]); i += 1
    parts.append("".join(cur))
    return re.compile("^" + r"%(?:\d\$)?(?:@|lld|llu|ld|d)".join(re.escape(p.replace("\\n", "\n")) for p in parts) + "$")

def scan(target: str, src_dir: pathlib.Path, shared: list):
    """查一个 target：它自己的目录加共享目录里的 Swift，对照它自己的字符串目录。"""
    catalog_path = src_dir / "Localizable.xcstrings"
    keys = set(json.loads(catalog_path.read_text(encoding="utf-8"))["strings"]) if catalog_path.exists() else set()
    used, problems = set(), []
    for path in sorted(p for d in [src_dir, *shared] for p in d.rglob("*.swift")):
        for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if IGNORE_MARK in line:
                continue
            code = line.split("//", 1)[0]
            if IGNORE.search(code):
                continue
            for start, lit in literals(code):
                if not CJK.search(lit):
                    continue
                before = code[:start]
                pat = key_pattern(lit)
                hits = [k for k in keys if pat.match(k)]
                used.update(hits)
                where = f"{path.relative_to(ROOT)}:{lineno}"
                if not (SWIFTUI_AUTO.search(before) or WRAPPED.search(before)):
                    problems.append(f"{where}: 未本地化的字面量 \"{lit}\"")
                if not hits:
                    problems.append(f"{where}: 目录里没有这个键 \"{lit}\"")
    for k in sorted(keys - used):
        problems.append(f"{catalog_path.relative_to(ROOT)}: 键没被代码用到（可能是预留或已失效）\"{k}\"")
    return problems

def main() -> int:
    bad = 0
    for target, (src_dir, shared) in TARGETS.items():
        problems = scan(target, src_dir, shared)
        print(f"== {target}: {len(problems)} 条")
        for p in problems:
            print("  " + p)
        bad += len(problems)
    return 1 if bad else 0

if __name__ == "__main__":
    sys.exit(main())
