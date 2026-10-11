// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <cstddef>
#include <span>

#include "common/enum.h"
#include "common/types.h"

namespace VideoCore {

enum ImageFlagBits : u32 {
    Empty = 0,
    MaybeCpuDirty = 1 << 0, ///< Image memory may have changed on an unprotected page
    CpuDirty = 1 << 1,      ///< Contents have been modified from the CPU
    GpuDirty = 1 << 2, ///< Contents have been modified from the GPU (valid data in buffer cache)
    Dirty = MaybeCpuDirty | CpuDirty | GpuDirty,
    GpuModified = 1 << 3, ///< Contents have been modified from the GPU
    Aliased = 1 << 4,     ///< Image shares its guest storage with another compatible image
    Registered = 1 << 6,  ///< True when the image is registered
    Picked = 1 << 7,      ///< Temporary flag to mark the image as picked
};
DECLARE_ENUM_FLAG_OPERATORS(ImageFlagBits)

/// Small images must be snapshotted before unprotecting their first page, not their last.
constexpr bool ImageFitsTwoPages(VAddr address, size_t size, size_t page_size) {
    return size != 0 && size <= 2 * page_size - address % page_size;
}

/// Capture the whole guest range before releasing protection. Repeated marks in the same
/// episode must not replace the snapshot with bytes the CPU may already have changed.
void MarkImageMaybeCpuDirty(ImageFlagBits& flags, u64& reference_hash,
                            std::span<const u8> guest_memory);

/// Resolve a speculative CPU invalidation. Returns true only when it becomes a confirmed
/// CPU write, so the caller can invalidate alias authority before resolving alias copies.
/// Explicit CPU/GPU invalidations must proceed through their normal upload paths.
[[nodiscard]] bool ResolveImageMaybeCpuDirty(ImageFlagBits& flags, u64 reference_hash,
                                             std::span<const u8> guest_memory);

} // namespace VideoCore
