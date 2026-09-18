# Sidecar staging — one owner action, not a decision

The verification helper (`crates/paraeq-stimulus`) has to be COPIED into the app
bundle. The bundler key that does it is verified and written out below; it is
**not yet in `tauri.conf.json`**, and this file says exactly why and exactly
what to add.

## The entry

```json
"bundle": {
  "externalBin": ["binaries/paraeq-stimulus"]
}
```

Verified against the current Tauri 2 documentation (`develop/sidecar.mdx`,
`learn/sidecar-nodejs.mdx`):

- `bundle.externalBin` is still the Tauri 2 key, and its paths are absolute or
  relative to `src-tauri`.
- The file on disk must carry the host target-triple suffix — the bundler is
  what appends it, so the entry stays unsuffixed. On Apple Silicon the staged
  file is **`binaries/paraeq-stimulus-aarch64-apple-darwin`**, and the triple
  comes from `rustc --print host-tuple`.
- Signing and notarization: the bundler signs sidecars inside-out and the
  sidecar needs no separate entitlements entry, but it *is* a signed payload.
  That blocks release packaging, not development.

## Why it is not in the config yet

`tauri-build` resolves `externalBin` **in the build script**, on every
`cargo build` — not only on `tauri build`. With the entry present and nothing
staged, `cargo build --workspace` and `cargo test --workspace` fail for
everyone, including CI, with `resource path ... doesn't exist`.

Staging it automatically is not available: cargo has no stable way to depend on
another crate's BINARY (artifact dependencies are nightly), and a build script
that shells out to `cargo build -p paraeq-stimulus` deadlocks on the same
workspace's target-directory lock.

So the entry lands with the staging, in one step, by the person who runs the
first real build.

## The staging

```bash
cargo build --release -p paraeq-stimulus
cp target/release/paraeq-stimulus \
   desktop/src-tauri/binaries/paraeq-stimulus-$(rustc --print host-tuple)
```

Staged binaries are build output and are gitignored.

## Then close the one open question

The Tauri docs state the key, the path resolution and the suffix. They do **not**
state the in-bundle destination path. The wizard spec assumes
`Contents/MacOS/paraeq-stimulus`, and the runtime resolver is written against
that assumption: it looks for the helper beside the running executable, which is
`Contents/MacOS/` in a bundle and the cargo target directory in dev
(`resolve_helper` in `../src/verify_seam.rs`;
`the_helper_path_resolves_in_dev_and_in_a_bundle` pins both branches).

```bash
cd desktop/ui && npm run tauri build
ls target/release/bundle/macos/ParaEQ.app/Contents/MacOS/
```

Paste the listing into the PR. **If the helper lands anywhere else, only
`resolve_helper` changes** — nothing else in the verification path depends on
the layout.

## Why `externalBin` and not `tauri-plugin-shell`

The bundler entry's only job is to COPY the binary. The app launches it with
plain `std::process::Command` (`../src/verify_seam.rs`), exactly as it already
launches `afplay` in `setup.rs`. `tauri-plugin-shell`'s `app.shell().sidecar()`
exists so the FRONTEND can launch processes, which is a permission surface this
app has no reason to open — so neither the plugin nor a `shell:allow-execute`
capability is present, and neither should be added for this.
