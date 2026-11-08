"""Package one clean Windows commit with licenses and offline-buildable source."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import zipfile


LICENSE_CHOICES = {
    "MIT": "MIT", "Zlib": "Zlib", "BSD-2-Clause": "BSD-2-Clause",
    "BSD-3-Clause": "BSD-3-Clause", "GPL-2.0": "GPL-2.0-only",
    "GPL-2.0-or-later": "GPL-2.0-only",
    "MIT OR Apache-2.0": "MIT", "Apache-2.0 OR MIT": "MIT",
    "0BSD OR MIT OR Apache-2.0": "0BSD", "Unlicense OR MIT": "MIT",
    "Zlib OR Apache-2.0 OR MIT": "Zlib", "BSD-3-Clause OR Apache-2.0": "BSD-3-Clause",
    "MIT OR Apache-2.0 OR Zlib": "MIT", "MIT OR Zlib OR Apache-2.0": "MIT",
    "MIT OR Apache-2.0 OR LGPL-2.1-or-later": "MIT",
    "(MIT OR Apache-2.0) AND IJG": "MIT AND IJG",
    "(MIT OR Apache-2.0) AND Unicode-3.0": "MIT AND Unicode-3.0",
    "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT": "MIT",
}


def run(*args, cwd):
    return subprocess.check_output(args, cwd=cwd, text=True, encoding="utf-8").strip()


def clean_commit(repo, expected):
    if not re.fullmatch(r"[0-9a-f]{40}", expected):
        raise ValueError("Expected commit must be a full lowercase Git SHA")
    if run("git", "rev-parse", "HEAD", cwd=repo) != expected:
        raise ValueError("HEAD differs from the expected release commit")
    if run("git", "status", "--porcelain", "--untracked-files=all", cwd=repo):
        raise ValueError("Release checkout contains uncommitted or untracked files")


def inventory(metadata):
    packages = []
    for package in metadata["packages"]:
        if not package["license"] and not package.get("license_file"):
            raise ValueError(f"Missing license information: {package['name']}")
        selected = LICENSE_CHOICES.get(package["license"])
        if selected is None:
            raise ValueError(f"Review license before distribution: {package['name']}: {package['license']}")
        entry = {key: package.get(key) for key in
                 ("name", "version", "license", "repository", "source")}
        entry["distribution_license"] = selected
        packages.append(entry)
    return sorted(packages, key=lambda item: (item["name"], item["version"]))


def write_zip(folder, destination):
    with zipfile.ZipFile(destination, "x", zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
        for path in sorted(folder.rglob("*")):
            if path.is_symlink():
                raise ValueError(f"Symlink cannot be packaged: {path.name}")
            if path.is_file():
                archive.write(path, (Path(folder.name) / path.relative_to(folder)).as_posix())


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-commit", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    clean_commit(repo, args.expected_commit)
    rustc = run("rustc", "--version", "--verbose", cwd=repo)
    target = "x86_64-pc-windows-msvc"
    if sys.platform != "win32" or f"host: {target}" not in rustc:
        raise ValueError(f"Packaging requires a native {target} toolchain")
    if os.environ.get("RUSTFLAGS") or os.environ.get("CARGO_ENCODED_RUSTFLAGS"):
        raise ValueError("Clear custom Rust flags before packaging")
    stage = args.output.resolve()
    stage.mkdir(parents=True, exist_ok=False)
    source = stage / "source" / "prismestra-source"
    source.mkdir(parents=True)
    git_archive = stage / "tracked-source.zip"
    subprocess.run(["git", "archive", "--format=zip", f"--output={git_archive}",
                    args.expected_commit], cwd=repo, check=True)
    with zipfile.ZipFile(git_archive) as archive:
        archive.extractall(source)
    config = source / ".cargo" / "config.toml"
    if config.exists():
        raise ValueError("Review the existing Cargo config before vendoring a release")
    vendor_config = run("cargo", "vendor", "--locked", "--offline", "--versioned-dirs",
                        "vendor/dependencies", cwd=source)
    config.parent.mkdir(exist_ok=True)
    config.write_text(vendor_config + "\n", encoding="utf-8")
    metadata = json.loads(run("cargo", "metadata", "--locked", "--offline",
                              "--format-version", "1", cwd=source))
    packages = inventory(metadata)
    build = stage / "build"
    build_env = os.environ.copy()
    build_env["CARGO_ENCODED_RUSTFLAGS"] = f"--remap-path-prefix={source}=."
    subprocess.run(["cargo", "build", "--release", "--locked", "--offline",
                    "--target", target, "--target-dir", str(build),
                    "--bin", "pc-control-ui"], cwd=source, env=build_env, check=True)
    clean_commit(repo, args.expected_commit)
    binary = stage / "binary" / "Prismestra"
    binary.mkdir(parents=True)
    executable = binary / "Prismestra.exe"
    shutil.copy2(build / target / "release" / "pc-control-ui.exe", executable)
    shutil.copy2(source / "LICENSE", binary / "LICENSE")
    notices = binary / "licenses"
    for package in metadata["packages"]:
        root = Path(package["manifest_path"]).parent
        candidates = set(root.glob("LICENSE*")) | set(root.glob("COPYING*")) | set(root.glob("NOTICE*"))
        if package.get("license_file"):
            candidates.add(root / package["license_file"])
        for path in sorted(candidates):
            if path.is_file():
                destination = notices / f"{package['name']}-{package['version']}" / path.name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, destination)
    for path in (source / "src").rglob("*-LICENSE*"):
        destination = notices / "project" / path.relative_to(source)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
    version = next(package["version"] for package in packages if package["name"] == "pc-control")
    stem = f"prismestra-{version}-{args.expected_commit[:12]}"
    source_zip = stage / f"{stem}-source.zip"
    manifest = {
        "product": "Prismestra", "version": version, "commit": args.expected_commit,
        "distribution_license": "GPL-2.0-only",
        "target": target, "rustc": rustc, "cargo": run("cargo", "--version", cwd=repo),
        "rustflags": ["--remap-path-prefix=<source>=."],
        "executable_sha256": sha256(executable), "source_archive": source_zip.name,
        "packages": packages,
    }
    for folder in (source, binary):
        (folder / "release-manifest.json").write_text(
            json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    (binary / "START-HERE.md").write_text(
        "# Prismestra\n\nRun `Prismestra.exe` to open the editor.\n\n"
        "Inspect and configure a supported connection before activating hardware. "
        "Keep your existing fan and pump control software.\n\n"
        "Device support and limitations: https://github.com/malyarq/prismestra/blob/"
        f"{args.expected_commit}/docs/compatibility.md\n\n"
        f"Corresponding source and dependency sources: `{source_zip.name}`, "
        "distributed alongside this package. See LICENSE and licenses/ for terms.\n",
        encoding="utf-8")
    write_zip(source, source_zip)
    binary_zip = stage / f"{stem}-{target}.zip"
    write_zip(binary, binary_zip)
    (stage / "SHA256SUMS.txt").write_text(
        "".join(f"{sha256(path)}  {path.name}\n" for path in (binary_zip, source_zip)),
        encoding="utf-8")
    print(f"Packaged commit {args.expected_commit}\n{binary_zip}\n{source_zip}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(f"Release packaging failed: {error}")
