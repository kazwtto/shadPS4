// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <array>
#include <gtest/gtest.h>

#include "video_core/renderer_vulkan/metadata_clear.h"
#include "video_core/texture_cache/image_access.h"

namespace {
struct State {
    vk::PipelineStageFlags2 pl_stage{};
    vk::AccessFlags2 access_mask{};
    vk::ImageLayout layout = vk::ImageLayout::eGeneral;
};
constexpr auto ColorWrite = vk::AccessFlagBits2::eColorAttachmentWrite;
constexpr auto DepthWrite = vk::AccessFlagBits2::eDepthStencilAttachmentWrite;
constexpr auto Read = vk::AccessFlagBits2::eShaderRead;
constexpr auto Fragment = vk::PipelineStageFlagBits2::eFragmentShader;
constexpr auto Color = vk::PipelineStageFlagBits2::eColorAttachmentOutput;

TEST(ImageAccessTest, OrdersFeedbackDrawsWithUnchangedState) {
    // A rendering scope contains a draw that samples and writes an attachment, followed by
    // another draw with the identical bindings. Equality of bindings does not make it read/read.
    const State state{Fragment | Color, Read | ColorWrite};
    EXPECT_FALSE(VideoCore::IsImageAccessRedundant(state, state.layout, state.access_mask,
                                                  state.pl_stage));
}

TEST(ImageAccessTest, OrdersSampledReadAndStorageReadAfterDepthWrites) {
    for (auto read : {vk::AccessFlagBits2::eShaderSampledRead,
                      vk::AccessFlagBits2::eShaderStorageRead}) {
        const State state{vk::PipelineStageFlagBits2::eAllCommands, read | DepthWrite};
        EXPECT_FALSE(VideoCore::IsImageAccessRedundant(state, state.layout, state.access_mask,
                                                      Fragment));
    }
}

TEST(ImageAccessTest, KeepsAttachmentOnlyDrawsInTheRenderingScope) {
    const State state{Color, ColorWrite | vk::AccessFlagBits2::eColorAttachmentRead,
                      vk::ImageLayout::eColorAttachmentOptimal};
    EXPECT_TRUE(VideoCore::IsImageAccessRedundant(state, state.layout, state.access_mask, Color));
}

TEST(ImageAccessTest, KeepsReadReadCheapAfterTheWriteWasMadeVisible) {
    const State state{Fragment, Read, vk::ImageLayout::eShaderReadOnlyOptimal};
    EXPECT_TRUE(VideoCore::IsImageAccessRedundant(state, state.layout, Read, Fragment));
}

TEST(ImageAccessTest, ReadAtAnotherStageNeedsVisibility) {
    const State state{Fragment, Read};
    EXPECT_FALSE(VideoCore::IsImageAccessRedundant(
        state, state.layout, Read, vk::PipelineStageFlagBits2::eVertexShader));
}

TEST(ImageAccessTest, GeneralLayoutStorageWritesStillRequireABarrier) {
    for (auto write : {vk::AccessFlagBits2::eShaderWrite,
                       vk::AccessFlagBits2::eShaderStorageWrite,
                       vk::AccessFlagBits2::eTransferWrite}) {
        const State state{vk::PipelineStageFlagBits2::eAllCommands, write};
        EXPECT_FALSE(VideoCore::IsImageAccessRedundant(state, state.layout, write, Fragment));
    }
}

TEST(ImageAccessTest, SameAccessInAnotherLayoutIsNotRedundant) {
    const State state{Fragment, Read};
    EXPECT_FALSE(VideoCore::IsImageAccessRedundant(
        state, vk::ImageLayout::eShaderReadOnlyOptimal, Read, Fragment));
}

struct Buffer {
    bool is_written;
    bool metadata;
    bool special = false;
    bool IsSpecial() const { return special; }
};

template <size_t N>
bool CanClear(const std::array<Buffer, N>& buffers, bool images = false, bool xor_op = false) {
    return Vulkan::CanEmulateMetadataClear(buffers, images, xor_op,
                                           [&](size_t i) { return buffers[i].metadata; });
}

TEST(MetadataClearTest, AcceptsMetadataOutputWithOrdinaryReadOnlyConstants) {
    EXPECT_TRUE(CanClear(std::array{Buffer{false, false}, Buffer{true, true}}));
}

TEST(MetadataClearTest, DoesNotDropAnotherWrittenBuffer) {
    // The previous first-match early return discarded the second buffer's write.
    EXPECT_FALSE(CanClear(std::array{Buffer{true, true}, Buffer{true, false}}));
    EXPECT_FALSE(CanClear(std::array{Buffer{true, false}, Buffer{true, true}}));
}

TEST(MetadataClearTest, AcceptsMultipleMetadataOutputsOnlyAsAWholeDispatch) {
    EXPECT_TRUE(CanClear(std::array{Buffer{true, true}, Buffer{true, true}}));
}

TEST(MetadataClearTest, RejectsMetadataCopiesAndReadModifyWriteDependencies) {
    EXPECT_FALSE(CanClear(std::array{Buffer{false, true}, Buffer{true, true}}));
}

TEST(MetadataClearTest, DoesNotDropImageOperations) {
    EXPECT_FALSE(CanClear(std::array{Buffer{true, true}}, true));
}

TEST(MetadataClearTest, RetainsTheAddressCalculationExclusion) {
    EXPECT_FALSE(CanClear(std::array{Buffer{true, true}}, false, true));
}

TEST(MetadataClearTest, DoesNotDropSpecialBufferWritesSuchAsGds) {
    EXPECT_FALSE(CanClear(std::array{Buffer{true, true}, Buffer{true, false, true}}));
}

TEST(MetadataClearTest, AllowsReadOnlySpecialBuffersWithoutTreatingThemAsGuestAddresses) {
    const std::array buffers{Buffer{false, false, true}, Buffer{true, true}};
    size_t queries = 0;
    EXPECT_TRUE(Vulkan::CanEmulateMetadataClear(buffers, false, false, [&](size_t i) {
        ++queries;
        EXPECT_EQ(i, 1u);
        return buffers[i].metadata;
    }));
    EXPECT_EQ(queries, 1u);
}

TEST(MetadataClearTest, RequiresAnActualMetadataOutput) {
    EXPECT_FALSE(CanClear(std::array{Buffer{false, false}}));
    EXPECT_FALSE(CanClear(std::array<Buffer, 0>{}));
}
} // namespace
