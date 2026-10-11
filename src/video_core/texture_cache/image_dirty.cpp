// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <xxhash.h>

#include "video_core/texture_cache/image_dirty.h"

namespace VideoCore {

void MarkImageMaybeCpuDirty(ImageFlagBits& flags, u64& reference_hash,
                            std::span<const u8> guest_memory) {
    if (False(flags & ImageFlagBits::MaybeCpuDirty)) {
        reference_hash = XXH3_64bits(guest_memory.data(), guest_memory.size());
    }
    flags |= ImageFlagBits::MaybeCpuDirty;
}

bool ResolveImageMaybeCpuDirty(ImageFlagBits& flags, u64 reference_hash,
                               std::span<const u8> guest_memory) {
    if (False(flags & ImageFlagBits::MaybeCpuDirty) ||
        True(flags & (ImageFlagBits::CpuDirty | ImageFlagBits::GpuDirty))) {
        return false;
    }
    const bool changed = reference_hash != XXH3_64bits(guest_memory.data(), guest_memory.size());
    flags &= ~ImageFlagBits::MaybeCpuDirty;
    if (changed) {
        flags |= ImageFlagBits::CpuDirty;
    }
    return changed;
}

} // namespace VideoCore
