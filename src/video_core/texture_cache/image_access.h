// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include "video_core/renderer_vulkan/vk_common.h"

namespace VideoCore {

template <typename State>
bool IsImageAccessRedundant(const State& state, vk::ImageLayout layout,
                           vk::AccessFlags2 access, vk::PipelineStageFlags2 stages) {
    constexpr auto explicit_writes = vk::AccessFlagBits2::eTransferWrite |
                                     vk::AccessFlagBits2::eShaderWrite |
                                     vk::AccessFlagBits2::eShaderStorageWrite |
                                     vk::AccessFlagBits2::eMemoryWrite;
    constexpr auto attachment_writes = vk::AccessFlagBits2::eColorAttachmentWrite |
                                       vk::AccessFlagBits2::eDepthStencilAttachmentWrite;
    constexpr auto shader_reads = vk::AccessFlagBits2::eShaderRead |
                                  vk::AccessFlagBits2::eShaderSampledRead |
                                  vk::AccessFlagBits2::eShaderStorageRead;
    const bool stage_visible = (state.pl_stage & vk::PipelineStageFlagBits2::eAllCommands) ||
                               (state.pl_stage & stages) == stages;
    // Attachment-only draws can share a rendering scope. A shader consumer needs a memory
    // dependency even if a feedback loop keeps exactly the same layout/access/stage tuple.
    // Check the last access, not sticky write history: read/read after a barrier stays cheap.
    const bool feedback_read = (state.access_mask & attachment_writes) && (access & shader_reads);
    // Read after read in the same layout needs no barrier once the stages already saw the
    // last write or layout change: there is nothing to make visible. Exact access matching
    // alone made a read-only depth attachment that is also sampled ping-pong every draw.
    constexpr auto all_writes = explicit_writes | attachment_writes;
    if (state.layout == layout && !(access & all_writes) && !(state.access_mask & all_writes) &&
        stages && (state.read_visible & stages) == stages) {
        return true;
    }
    return state.layout == layout && state.access_mask == access && stage_visible &&
           !(state.access_mask & explicit_writes) && !feedback_read;
}

} // namespace VideoCore
