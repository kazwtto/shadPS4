// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <cstdint>
#include <span>

#include "video_core/renderer_vulkan/vk_common.h"

namespace VideoCore {

// Subresource states use mip-major order, then array layer. Do not infer the selected
// view's layout from the backing's summary when other subresources have different states.
template <typename State, std::size_t Extent>
bool SubresourcesHaveLayout(std::span<const State, Extent> states, std::uint32_t total_levels,
                            std::uint32_t total_layers, std::uint32_t base_level,
                            std::uint32_t base_layer, std::uint32_t levels, std::uint32_t layers,
                            vk::ImageLayout layout) {
    if (!levels || !layers || base_level >= total_levels || base_layer >= total_layers ||
        levels > total_levels - base_level || layers > total_layers - base_layer ||
        states.size() != std::size_t(total_levels) * total_layers) {
        return false;
    }
    for (std::uint32_t level = base_level; level < base_level + levels; ++level) {
        for (std::uint32_t layer = base_layer; layer < base_layer + layers; ++layer) {
            if (states[std::size_t(level) * total_layers + layer].layout != layout) {
                return false;
            }
        }
    }
    return true;
}

} // namespace VideoCore
