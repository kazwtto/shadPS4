// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <cstddef>

namespace Vulkan {

// Skipping a dispatch replaces ALL its outputs. Validate every resource before changing any
// clear masks; a metadata write alone does not make unrelated buffer/image writes disposable.
template <typename Buffers, typename IsMetadata>
bool CanEmulateMetadataClear(const Buffers& buffers, bool has_images, bool has_bitwise_xor,
                            IsMetadata is_metadata) {
    if (has_images || has_bitwise_xor) {
        return false;
    }
    bool has_metadata_write = false;
    for (size_t i = 0; i < buffers.size(); ++i) {
        const auto& buffer = buffers[i];
        if (buffer.IsSpecial()) {
            if (buffer.is_written) {
                return false;
            }
            continue;
        }
        const bool metadata = is_metadata(i);
        if (buffer.is_written) {
            if (!metadata) {
                return false;
            }
            has_metadata_write = true;
        } else if (metadata) {
            return false;
        }
    }
    return has_metadata_write;
}

} // namespace Vulkan
