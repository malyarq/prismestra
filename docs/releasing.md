# Building a release package

Use a clean checkout of the exact commit whose Windows checks and acceptance results you intend to release. Packaging requires Python 3.11+, Git, Cargo and a native `x86_64-pc-windows-msvc` toolchain. It does not activate hardware or install the application.

```powershell
cargo fetch --locked
python scripts/test_package_release.py
$releaseCommit = git rev-parse HEAD
python scripts/package_release.py --expected-commit $releaseCommit --output C:\builds\prismestra-release
```

The output directory must not already exist. Use a location outside the checkout or inside its ignored `target` directory. A failed attempt leaves its staging files for inspection; choose a new directory when retrying.

The script archives tracked files at the expected commit, vendors the locked registry dependencies, adds a local Cargo source replacement, and builds that source snapshot offline. It rejects a dirty checkout and custom Rust flags, remaps source paths in compiled diagnostics, and checks HEAD again after building. The source archive excludes build outputs and local configuration. See the [implementation](../scripts/package_release.py).

Distribute these files together:

- `prismestra-<version>-<commit>-x86_64-pc-windows-msvc.zip`: application, license texts, launch notes and manifest.
- `prismestra-<version>-<commit>-source.zip`: the same tracked source plus dependency sources and an offline Cargo configuration.
- `SHA256SUMS.txt`: hashes of both archives.

`release-manifest.json` records the exact commit, target, compiler, Cargo version, executable hash, upstream license expressions and the selected license alternatives for this GPL-2.0 distribution. It excludes local Cargo paths and package IDs. Upstream notices are retained; a license identifier alone does not replace the license text. The [explicit selection table](../scripts/package_release.py) rejects unreviewed expressions. It chooses permissive alternatives where available without changing the original source license grants.

Apache-2.0-only dependencies cannot be combined into this GPL-2.0-only distribution. See the [Apache Software Foundation compatibility explanation](https://www.apache.org/licenses/GPL-compatibility.html) and [GNU license list](https://www.gnu.org/licenses/license-list.html#apache2). Choosing GPL-3.0 for original project files would not relicense a GPL-2.0-only dependency.

To rebuild from the source archive, install the recorded compiler/toolchain and Windows build tools, then run `cargo build --release --locked --offline --target x86_64-pc-windows-msvc --bin pc-control-ui` inside `prismestra-source`. This records reproducible inputs and commands; byte-identical executable output across machines has not been established.

Before publishing, test the packaged executable with a clean state directory, verify its checksum, and complete the applicable editor, engine lifecycle, device and resource checks. Passing software CI does not certify physical devices. Never package credentials or private hardware bindings. Release artifacts must match the tested final commit.
