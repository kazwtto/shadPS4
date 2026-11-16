// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <algorithm>
#include <cstddef>

namespace Vulkan {

// Transitions are stage-local, whereas descriptor bindings survive across stages of a draw.
// Only the newly appended bindings can use the current transition indices. Older descriptors
// of a shared image must still receive its final layout when a later stage changes that layout.
template <typename Bindings, typename Transitions, typename Infos>
void UpdateTextureDescriptorLayouts(const Bindings& bindings, size_t first_stage_descriptor,
                                    const Transitions& transitions, Infos& infos) {
    for (size_t i = 0; i < first_stage_descriptor; ++i) {
        const auto& binding = bindings[i];
        const auto it =
            std::find_if(transitions.begin(), transitions.end(),
                         [&](const auto& entry) { return entry.image_id == binding.image_id; });
        if (it != transitions.end()) {
            infos[binding.descriptor_index].imageLayout = it->layout;
        }
    }
    for (size_t i = first_stage_descriptor; i < bindings.size(); ++i) {
        const auto& binding = bindings[i];
        infos[binding.descriptor_index].imageLayout = transitions[binding.transition_index].layout;
    }
}

} // namespace Vulkan
