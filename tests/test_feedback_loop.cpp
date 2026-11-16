// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <vector>
#include <gtest/gtest.h>

#include "video_core/renderer_vulkan/feedback_loop.h"

namespace Vulkan {
namespace {

TEST(FeedbackLoop, VegaStaticModeDoesNotRequireDynamicExtension) {
    std::vector states{vk::DynamicState::eViewportWithCount};
    AddFeedbackLoopDynamicState(states, true, false);
    EXPECT_EQ(states.size(), 1);
    EXPECT_EQ(FeedbackLoopPipelineFlags(true, false),
              vk::PipelineCreateFlags{vk::PipelineCreateFlagBits::eColorAttachmentFeedbackLoopEXT});
}

TEST(FeedbackLoop, DynamicModeDeclaresTheStateUsedByTheCommand) {
    std::vector states{vk::DynamicState::eViewportWithCount, vk::DynamicState::eScissorWithCount};
    AddFeedbackLoopDynamicState(states, true, true);
    ASSERT_EQ(states.size(), 3);
    EXPECT_EQ(states[0], vk::DynamicState::eViewportWithCount);
    EXPECT_EQ(states[1], vk::DynamicState::eScissorWithCount);
    EXPECT_EQ(states[2], vk::DynamicState::eAttachmentFeedbackLoopEnableEXT);
    EXPECT_FALSE(FeedbackLoopPipelineFlags(true, true));
}

TEST(FeedbackLoop, MissingFeaturesDoNotEnableEitherPath) {
    std::vector<vk::DynamicState> states;
    AddFeedbackLoopDynamicState(states, false, false);
    EXPECT_TRUE(states.empty());
    EXPECT_FALSE(FeedbackLoopPipelineFlags(false, false));
}

TEST(FeedbackLoop, DynamicFeatureAloneCannotEnableUnsupportedLayout) {
    std::vector<vk::DynamicState> states;
    AddFeedbackLoopDynamicState(states, false, true);
    EXPECT_TRUE(states.empty());
    EXPECT_FALSE(FeedbackLoopPipelineFlags(false, true));
}

} // namespace
} // namespace Vulkan
