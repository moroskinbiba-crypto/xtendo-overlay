# NX Resource Toolkit

A small Windows-first desktop utility for preparing an already-available Horizon NX resource directory for a Nintendo Switch SD card.

> **Scope of this MVP:** validate an existing prepared resource directory, build the Switch-compatible `resources.hzpak`, and install both to the SD card with a recoverable backup. It does **not** download Minecraft, obtain proprietary game files, perform account/licence checks, or generate Minecraft's proprietary source assets from scratch.

## Supported input

Choose the directory whose root contains:

```text
prepared-resources/
├── horizon-resources.json
├── assets/
├── data/
└── ... other generated resource files
```

The tool checks that the manifest is valid JSON, the expected top-level directories exist, and the tree contains no symbolic links or paths that cannot be represented by the pack format.

## Output on the SD card

Select the root of the Switch SD card (the directory containing `switch/`). Deployment writes:

```text
SD:/
└── switch/
    ├── horizon/
    │   ├── resources/
    │   │   ├── horizon-resources.json
    │   │   ├── assets/
    │   │   ├── data/
    │   │   └── ...
    │   └── resources.hzpak
    └── horizon-backups/
        └── <timestamp>-<pid>/
            ├── resources/       # previous folder, when present
            └── resources.hzpak # previous pack, when present
```

Existing resources are moved to a backup before the new directory is activated. Temporary files are built first, and the pack footer/index is checked before deployment. The app never deletes previous backups.

## Build on Windows

Install the stable Rust toolchain from [rustup.rs](https://rustup.rs), then run:

```powershell
cargo build --release
```

The executable is written to `target\release\nx-resource-toolkit.exe`.

## Current status

The first implementation provides:
- resource-directory validation and size/count reporting;
- HZPAK-compatible pack creation (files up to 8 MiB are packed; larger files remain loose);
- SD-card detection by the presence of a `switch/` directory;
- staged deployment and backup/rollback of the previous resource folder and pack;
- a Windows x64 GitHub Actions build artifact.

Not implemented yet: generating `assets/` and `data/` from Minecraft's original client/server files, NRO installation, automatic Companion migration, mod/pack management, and world backup/sync.

## Compatibility notes

The destination layout follows the Horizon NX 2.x installation instructions. The HZPAK container layout is compatible with the format used by Horizon NX / Horizon NX Companion. Keep a backup of your SD card before testing any new homebrew tooling.

## License and attribution

This project is released under LGPL-3.0-or-later. The HZPAK writer follows the publicly available Horizon NX pack layout and acknowledges the implementation and format notes in [Horizon NX Companion](https://github.com/KoHaku743/horizon-nx-companion), also LGPL-3.0-or-later. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
