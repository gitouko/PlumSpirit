# Building PlumSpirit from source

Users who only want to run the app should use the latest verified Windows Portable ZIP on GitHub Releases instead.

## Windows (recommended for current release)

Install Rust stable **1.95 or newer**, and Visual Studio Build Tools with the **Desktop development with C++** workload.

From the repository root:

```powershell
cargo build --locked --release -p plumspirit --bin PlumSpirit
```

The resulting executable is expected at `target\release\PlumSpirit.exe`. Depending on the native compiler/runtime setup, the built executable may require the Microsoft Visual C++ runtime or the permitted application-local redistributable DLLs. Check the binary's imports and Microsoft's redistribution terms; do not assume every MSVC build is self-contained.

The normal portable build also needs a suitable `.sf2` SoundFont for internal audio. Refer to `THIRD_PARTY_NOTICES.md` before redistributing third-party files. The `.sf2` is not included in the source ZIP prepared for review.

The project's existing automated checks live in the Rust workspace and `tools/`. Example:

```powershell
cargo fmt --all -- --check
cargo test --locked --workspace
```

The source snapshot is not evidence that these commands have passed on your particular Windows computer. Review `docs/v1_0_launch/RELEASE_GATE.md` in the original development archive for the previous environment's results.
