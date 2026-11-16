// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include "video_core/renderer_vulkan/vk_common.h"

namespace Vulkan {

/// Static enablement permits feedback on supported color attachments without requiring
/// the optional dynamic-state extension. With dynamic state, the command controls enablement.
[[nodiscard]] constexpr vk::PipelineCreateFlags FeedbackLoopPipelineFlags(bool layout,
                                                                         bool dynamic) {
    return layout && !dynamic
               ? vk::PipelineCreateFlags{vk::PipelineCreateFlagBits::eColorAttachmentFeedbackLoopEXT}
               : vk::PipelineCreateFlags{};
}

template <typename States>
void AddFeedbackLoopDynamicState(States& states, bool layout, bool dynamic) {
    if (layout && dynamic) {
        states.push_back(vk::DynamicState::eAttachmentFeedbackLoopEnableEXT);
    }
}

} // namespace Vulkan
