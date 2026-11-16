<!--
SPDX-FileCopyrightText: 2026 shadPS4 Emulator Project
SPDX-License-Identifier: GPL-2.0-or-later
-->

# God of War III Launcher 

This repository packages a **God of War III Remastered-focused launcher** with a custom
shadPS4 build. The launcher's built-in game workflow targets **CUSA01623, update 01.02**:
it validates that title, prepares its patches and settings, and launches it in an isolated
environment.

You can select a custom shadPS4 executable in the launcher, so it can also serve as a
custom frontend for another shadPS4 build. The launcher itself remains dedicated to its
God of War III workflow; it is not a general game library.

The repository also changes shadPS4's memory and Vulkan paths. Some changes are gated to
recognized God of War III title IDs; others improve shared emulator internals.

> [!CAUTION]
> This is an unofficial project and is not endorsed by the upstream shadPS4 developers.
> Send issues about this build to this repository, with the game serial and build details.

## What this project adds

### A launcher built around God of War III

The included desktop app provides:

- Game and update validation for CUSA01623 / 01.02, including the expected game binary
  hash and original bytes at selected patch locations.
- An embedded catalog of **27 patch options** and built-in profiles. It includes texture
  correction, 480p through 4K resolution choices, a 120 FPS patch, and intro/video
  skipping. A separate group contains experimental changes for image effects, shadows,
  particles, geometry and LOD.
- Per-game shadPS4 settings, saved patch profiles, and selection of a custom shadPS4
  executable.
- A private runtime environment for the game. The app copies the selected executable,
  generates the patch XML for that session, applies temporary settings and restores them
  when the session ends. An interrupted session can be recovered from the app.
- Session activity, logs and optional diagnostic capture, plus a read-only trophy page
  and save import.

The launcher does not patch the original game executable in place. Its catalog and
manifest are embedded in the app; no external patch XML needs to be installed.

### Changes to the emulator used by the game

- **God of War III GPU-authority path.** For recognized GoW3 serials, GPU writes can stay
  authoritative in tracked shadow storage. Eligible GPU reads reuse that data, while a
  guest CPU read triggers materialization into guest memory. Virtual fences and range
  tracking preserve ordering across overlapping and partial writes. This targets the
  repeated GPU-to-guest-memory synchronization in the game's render path; it is not an
  FPS guarantee.
- **Asynchronous guest-memory copies.** Buffer uploads can move guest-memory copies to
  worker jobs. Sequence tracking, overlap checks and waits for protected guest writes keep
  those copies ordered with GPU work.
- **Texture readback and alias correctness.** Readback copies now track row pitch and
  write only texel spans, not row padding. Upload baselines preserve unchanged GPU-modified
  mip levels, and alias handling accounts for partial overlaps and different image
  layouts.
- **Windows memory commitment.** Physical backing pages are reserved first and committed
  as guest mappings need them, reducing the initial Windows commit charge.
- **Vulkan resource and pipeline handling.** The fork adds explicit feedback-loop and
  descriptor-layout handling, guards metadata clears, omits empty fragment stages in
  depth-only pipelines, and reuses scratch allocations only after their GPU work completes.
  Cache-budget calculations also handle small reported memory budgets safely.

### Execution and cache controls

The emulator exposes **precise**, **balanced** and **fast** GPU execution policies, plus
lazy pipeline-cache creation. These change pipeline compilation, cache and frame-backlog
policy; they keep shader accuracy and synchronization barriers intact. The launcher
exposes these controls only for a recognized executable hash. **Fast** can use more
memory and increase latency, and lazy creation can move work to the first use of a
pipeline. **No gameplay FPS benefit has been established for these options.**

## Validation status

The launcher catalog contains useful options, but availability is not proof of in-game
behavior. The patch manifest records **static verification only**: it checks expected
title/update data and original patch bytes. No patch in the current catalog is marked as
gameplay-validated. Treat the experimental entries as experiments and check each option
in game.

The project's performance target is **60 FPS at 720p**, using a Ryzen 7 5825U as the
reference machine. That is a target, not a measured or guaranteed result. The 120 FPS
patch is an option in the catalog; its presence does not establish that the game sustains
120 FPS.

## Build and launch

### Requirements

- Windows 10 or 11, 64-bit, with Vulkan drivers.
- Your own God of War III Remastered base game and update **CUSA01623 / 01.02**.
- A compatible shadPS4 executable. The repository contains source, not a prebuilt
  launcher or emulator executable, and does not include game files.
- Rust 1.82 or newer to build the launcher.

### Build the launcher

From the repository root:

    cargo build --release --manifest-path GOW3_Launcher/launcher-rs/Cargo.toml

The executable is created at:

    GOW3_Launcher/launcher-rs/target/release/gow3-launcher.exe

### Configure a game session

1. Open **Configurações → Caminhos**.
2. Select the base game, the 01.02 update, and the shadPS4 .exe.
3. Return to **Início**, verify the files, choose a profile, and review **Ajustes** before
   starting the game. The launcher reports the required additional DMEM for the selected
   patch options.
4. Use **Atividade e logs** to inspect a session. If the app reports an interrupted
   session, recover it before starting another one.

The launcher's catalog accepts only the title and update it was built for. A different
serial, update, or modified game binary will fail validation rather than receive patches
at offsets intended for CUSA01623 / 01.02.

### Build the emulator

To build the emulator executable from this checkout, use the
[shadPS4 Windows build instructions](https://github.com/shadps4-emu/shadPS4/blob/main/documents/building-windows.md)
and the build configuration in this repository. Then select the resulting .exe under
**Configurações → Caminhos → Executável do shadPS4**.

## Notes

- Save import is limited to the launcher's CUSA01623 environment and does not overwrite
  existing private saves.
- The trophy page reads progress without modifying it. If trophy definitions are still
  encrypted in the game package, provide the required key in
  ambiente/user/keys.json.
- The emulator's GPU-authority fast path is gated to recognized God of War III titles.
  The launcher's game catalog remains limited to CUSA01623 / 01.02.

## Showcase

<p align="center">
  <a href="https://www.youtube.com/watch?v=tWWtB59fE7o">
    <img
      src="https://img.youtube.com/vi/tWWtB59fE7o/maxresdefault.jpg"
      width="720"
      alt="God of War III launcher demonstration"
    >
  </a>
  <br>
  <a href="https://www.youtube.com/watch?v=tWWtB59fE7o">
    ▶️ <strong>Watch on YouTube</strong>
  </a>
</p>

## Source and license

This fork is based on the open-source [shadPS4 project](https://github.com/shadps4-emu/shadPS4)
and is available under the [GPL-2.0-or-later license](LICENSE).
