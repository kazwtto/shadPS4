// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <array>
#include <limits>

#include <gtest/gtest.h>

#include "video_core/texture_cache/attachment_layout.h"

namespace {

struct State {
    vk::ImageLayout layout;
};
constexpr auto Write = vk::ImageLayout::eDepthStencilAttachmentOptimal;
constexpr auto Read = vk::ImageLayout::eDepthStencilReadOnlyOptimal;

TEST(AttachmentLayoutTest, KeepsSelectedWritableLayerDespiteOtherReadOnlyLayers) {
    const std::array states{State{Read}, State{Write}, State{Read}};
    EXPECT_TRUE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 3, 0, 1, 1, 1, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 3, 0, 0, 1, 3, Write));
}

TEST(AttachmentLayoutTest, RejectsSampledOrReadOnlySelectedSubresource) {
    const std::array states{State{Write}, State{Read}};
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 2, 0, 1, 1, 1, Write));
}

TEST(AttachmentLayoutTest, ChecksAllMipsAndLayersInTheView) {
    const std::array states{State{Read}, State{Write}, State{Write},
                            State{Read}, State{Write}, State{Write}};
    EXPECT_TRUE(VideoCore::SubresourcesHaveLayout(std::span{states}, 2, 3, 0, 1, 2, 2, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 2, 3, 0, 0, 2, 2, Write));
}

TEST(AttachmentLayoutTest, UsesMipMajorRatherThanLayerMajorIndexing) {
    const std::array states{State{Read}, State{Read}, State{Read}, State{Write}};
    EXPECT_TRUE(VideoCore::SubresourcesHaveLayout(std::span{states}, 2, 2, 1, 1, 1, 1, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 2, 2, 1, 0, 1, 1, Write));
}

TEST(AttachmentLayoutTest, RejectsEmptyAndOutOfBoundsRanges) {
    const std::array states{State{Write}, State{Write}};
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 2, 0, 0, 0, 1, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 2, 0, 0, 1, 0, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 2, 1, 0, 1, 1, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 2, 0, 2, 1, 1, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 2, 0, 1, 1, 2, Write));
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(
        std::span{states}, 1, 2, 0, 0, std::numeric_limits<std::uint32_t>::max(), 1, Write));
}

TEST(AttachmentLayoutTest, RejectsIncompleteStateArrays) {
    const std::array states{State{Write}};
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 2, 2, 0, 0, 1, 1, Write));
    EXPECT_FALSE(
        VideoCore::SubresourcesHaveLayout(std::span<const State>{}, 1, 1, 0, 0, 1, 1, Write));
}

TEST(AttachmentLayoutTest, RequiresExactLayoutIncludingStencilMode) {
    const std::array states{State{vk::ImageLayout::eDepthReadOnlyStencilAttachmentOptimal}};
    EXPECT_FALSE(VideoCore::SubresourcesHaveLayout(std::span{states}, 1, 1, 0, 0, 1, 1, Write));
}

} // namespace
