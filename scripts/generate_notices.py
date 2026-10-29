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
    if not licenses and package["name"].startswith("webview2-com"):
        licenses = [root / "assets/licenses/webview2-rs-MIT.txt"]
    if not licenses:
        raise SystemExit(f"Missing license text for {package['name']}")
    sections.append(f"\n{'=' * 72}\n{package['name']} {package['version']}\n"
                    f"License: {package.get('license')}\nSource: {package.get('repository') or package['source']}\n")
    for license_file in licenses:
        label = license_file.relative_to(directory).as_posix() if license_file.is_relative_to(directory) else license_file.relative_to(root).as_posix()
        sections.append(f"\n--- {label} ---\n"
                        + license_file.read_text(encoding="utf-8", errors="replace"))
sections.append("\n" + "=" * 72 + "\nMicrosoft WebView2 SDK 1.0.3800.47: static loader\n")
for license_file in sorted((root / "assets/licenses").glob("Microsoft-WebView2-*.txt")):
    sections.append(f"\n--- {license_file.name} ---\n" + license_file.read_text(encoding="utf-8-sig"))
sections.append("\n" + "=" * 72 + "\nEmbedded frontend runtime and generated helpers (npm)\n")
for name in ["preact", "@preact/signals", "@preact/signals-core", "astro", "@astrojs/preact", "@astrojs/internal-helpers", "vite"]:
    directory = root / "ui/node_modules" / name
    manifest = json.loads((directory / "package.json").read_text(encoding="utf-8"))
    licenses = sorted(p for p in directory.iterdir() if p.is_file() and
                      p.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")))
    if not licenses:
        raise SystemExit(f"Missing npm license text for {name}")
    sections.append(f"\n{'=' * 72}\n{manifest['name']} {manifest['version']}\nLicense: {manifest.get('license')}\n")
    for license_file in licenses:
        sections.append(f"\n--- {license_file.name} ---\n" + license_file.read_text(encoding="utf-8", errors="replace"))
output = "\n".join(line.rstrip() for line in "\n".join(sections).splitlines()) + "\n"
(root / "THIRD_PARTY_NOTICES.txt").write_text(output, encoding="utf-8")
