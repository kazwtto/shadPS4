// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <array>
#include <cstdint>

namespace VideoCore {
struct MipUploadBaseline {
    std::array<std::uint64_t, 16> hashes{};
    std::uint16_t valid{};

    bool Preserve(unsigned mip, std::uint64_t hash, bool gpu_modified, bool gpu_dirty) const {
        return mip < hashes.size() && gpu_modified && !gpu_dirty &&
               (valid & (1u << mip)) && hashes[mip] == hash;
    }

    void Record(unsigned mip, std::uint64_t hash) {
        if (mip < hashes.size()) {
            hashes[mip] = hash;
            valid |= static_cast<std::uint16_t>(1u << mip);
        }
    }
};
} // namespace VideoCore
