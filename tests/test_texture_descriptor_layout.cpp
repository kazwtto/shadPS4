// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <vector>

#include <gtest/gtest.h>

#include "video_core/renderer_vulkan/texture_descriptor_layout.h"
#include "video_core/renderer_vulkan/vk_common.h"

namespace {
struct Binding {
    unsigned image_id;
    unsigned descriptor_index;
    unsigned transition_index;
};
struct Transition {
    unsigned image_id;
    vk::ImageLayout layout;
};

TEST(TextureDescriptorLayoutTest, UsesMergedLayoutForDuplicateCurrentStageBindings) {
    const std::vector<Binding> bindings{{1, 0, 0}, {1, 1, 0}, {2, 2, 1}};
    const std::vector<Transition> transitions{{1, vk::ImageLayout::eGeneral},
                                              {2, vk::ImageLayout::eShaderReadOnlyOptimal}};
    std::vector<vk::DescriptorImageInfo> infos(3);
    Vulkan::UpdateTextureDescriptorLayouts(bindings, 0, transitions, infos);
    EXPECT_EQ(infos[0].imageLayout, vk::ImageLayout::eGeneral);
    EXPECT_EQ(infos[1].imageLayout, vk::ImageLayout::eGeneral);
    EXPECT_EQ(infos[2].imageLayout, vk::ImageLayout::eShaderReadOnlyOptimal);
}

TEST(TextureDescriptorLayoutTest, DoesNotUseStaleIndicesFromAnEarlierStage) {
    const std::vector<Binding> bindings{{3, 0, 63}, {9, 1, 62}, {3, 2, 0}};
    const std::vector<Transition> transitions{{3, vk::ImageLayout::eGeneral}};
    std::vector<vk::DescriptorImageInfo> infos(3);
    infos[1].imageLayout = vk::ImageLayout::eDepthStencilReadOnlyOptimal;
    Vulkan::UpdateTextureDescriptorLayouts(bindings, 2, transitions, infos);
    EXPECT_EQ(infos[0].imageLayout, vk::ImageLayout::eGeneral);
    EXPECT_EQ(infos[1].imageLayout, vk::ImageLayout::eDepthStencilReadOnlyOptimal);
    EXPECT_EQ(infos[2].imageLayout, vk::ImageLayout::eGeneral);
}

TEST(TextureDescriptorLayoutTest, LeavesEarlierDescriptorsUntouchedForAnEmptyStage) {
    const std::vector<Binding> bindings{{7, 0, 63}};
    const std::vector<Transition> transitions;
    std::vector<vk::DescriptorImageInfo> infos(1);
    infos[0].imageLayout = vk::ImageLayout::eGeneral;
    Vulkan::UpdateTextureDescriptorLayouts(bindings, 1, transitions, infos);
    EXPECT_EQ(infos[0].imageLayout, vk::ImageLayout::eGeneral);
}

TEST(TextureDescriptorLayoutTest, MatchesImageSearchReferenceAcrossStageBoundaries) {
    const std::vector<Binding> bindings{{4, 0, 61}, {8, 1, 60}, {4, 2, 62},
                                        {8, 3, 0},  {5, 4, 1},  {4, 5, 2}};
    const std::vector<Transition> transitions{
        {8, vk::ImageLayout::eGeneral},
        {5, vk::ImageLayout::eShaderReadOnlyOptimal},
        {4, vk::ImageLayout::eAttachmentFeedbackLoopOptimalEXT}};
    std::vector<vk::DescriptorImageInfo> expected(6), actual(6);
    for (const auto& binding : bindings) {
        for (const auto& transition : transitions) {
            if (transition.image_id == binding.image_id) {
                expected[binding.descriptor_index].imageLayout = transition.layout;
                break;
            }
        }
    }
    Vulkan::UpdateTextureDescriptorLayouts(bindings, 3, transitions, actual);
    for (size_t i = 0; i < actual.size(); ++i) {
        EXPECT_EQ(actual[i].imageLayout, expected[i].imageLayout);
    }
}
} // namespace
