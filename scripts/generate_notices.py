import json
import pathlib
import subprocess

root = pathlib.Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"], cwd=root
))
sections = ["TgWsProxyPlus — third-party notices\n\n"
            "Includes all locked platform/build dependencies; not all are linked on every target.\n"
            "Rust standard library: MIT OR Apache-2.0, https://github.com/rust-lang/rust\n"]
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if not package["source"]:
        continue
    directory = pathlib.Path(package["manifest_path"]).parent
    licenses = sorted(p for p in directory.rglob("*") if p.is_file() and
                      (p.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE"))) and
                      "tests" not in p.relative_to(directory).parts)
    if not licenses and (directory / "AUTHORS").exists():
        licenses = [directory / "AUTHORS"]
    if not licenses:
        raise SystemExit(f"Missing license text for {package['name']}")
    sections.append(f"\n{'=' * 72}\n{package['name']} {package['version']}\n"
                    f"License: {package.get('license')}\nSource: {package.get('repository') or package['source']}\n")
    for license_file in licenses:
        sections.append(f"\n--- {license_file.relative_to(directory).as_posix()} ---\n"
                        + license_file.read_text(encoding="utf-8", errors="replace"))
(root / "THIRD_PARTY_NOTICES.txt").write_text("\n".join(sections), encoding="utf-8")
