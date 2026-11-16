// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <algorithm>
#include <vector>
#include <gtest/gtest.h>
#include <vulkan/vulkan.hpp>

#include "video_core/amdgpu/resource.h"
#include "video_core/texture_cache/aliasing.h"
#include "video_core/texture_cache/image_info.h"
#include "video_core/texture_cache/types.h"

namespace VideoCore {
namespace {

ImageInfo MakeImageInfo(Extent3D size, vk::Format format = vk::Format::eR8G8B8A8Unorm) {
    ImageInfo info{};
    info.guest_address = 0x100000;
    info.num_bits = 32;
    info.pixel_format = format;
    info.type = AmdGpu::ImageType::Color2D;
    info.pitch = size.width;
    info.size = size;
    return info;
}

TEST(AliasingTest, CopiesCoveredImageWithIdenticalFormat) {
    const ImageInfo src = MakeImageInfo({1920, 1088, 1});
    const ImageInfo dst = MakeImageInfo({1920, 1080, 1});

    EXPECT_EQ(GetAliasCopyExtent(src, dst), dst.size);
    EXPECT_EQ(GetAliasCopyExtent(dst, src), dst.size);
}

TEST(AliasingTest, CopiesIntersectionBetweenCompatibleViews) {
    const ImageInfo src = MakeImageInfo({1920, 1088, 1}, vk::Format::eR8G8B8A8Uint);
    const ImageInfo dst = MakeImageInfo({1920, 1080, 1}, vk::Format::eR8G8B8A8Unorm);

    const Extent3D intersection{1920, 1080, 1};
    EXPECT_EQ(GetAliasCopyExtent(src, dst), intersection);
    EXPECT_EQ(GetAliasCopyExtent(dst, src), intersection);
}

TEST(AliasingTest, RejectsDifferentMemoryLayouts) {
    const ImageInfo src = MakeImageInfo({1920, 1088, 1});
    ImageInfo dst = MakeImageInfo({1920, 1080, 1});

    dst.guest_address += 0x1000;
    EXPECT_FALSE(GetAliasCopyExtent(src, dst).has_value());

    dst = MakeImageInfo({1920, 1080, 1});
    ++dst.pitch;
    EXPECT_FALSE(GetAliasCopyExtent(src, dst).has_value());

    dst = MakeImageInfo({1920, 1080, 1});
    dst.props.is_depth = true;
    EXPECT_FALSE(GetAliasCopyExtent(src, dst).has_value());
}

TEST(AliasingTest, RejectsIncompatibleIntersectionCopies) {
    const ImageInfo src = MakeImageInfo({1920, 1088, 1}, vk::Format::eR8G8B8A8Uint);
    ImageInfo dst = MakeImageInfo({1280, 1080, 1}, vk::Format::eR8G8B8A8Unorm);
    EXPECT_FALSE(GetAliasCopyExtent(src, dst).has_value());

    dst = MakeImageInfo({1920, 1080, 1}, vk::Format::eR16G16B16A16Sfloat);
    EXPECT_FALSE(GetAliasCopyExtent(src, dst).has_value());

    dst = MakeImageInfo({1920, 1080, 1}, vk::Format::eR8G8B8A8Unorm);
    dst.resources.levels = 2;
    EXPECT_FALSE(GetAliasCopyExtent(src, dst).has_value());
}

TEST(AliasingTest, MergesSingleTexelWriterIntoWideLinearBacking) {
    auto backing = MakeImageInfo({128, 1, 1}, vk::Format::eR16G16Sfloat);
    auto writer = backing;
    writer.size = {1, 1, 1};
    EXPECT_EQ(GetAliasCopyExtent(writer, backing), writer.size);
    EXPECT_EQ(GetAliasCopyExtent(backing, writer), writer.size);
    EXPECT_TRUE(RequiresSeparateAliasImage(backing, writer));
}

TEST(AliasingTest, SameByteSizeDoesNotMakeDifferentGeometryAView) {
    auto cached = MakeImageInfo({128, 1, 1});
    cached.guest_size = 512;
    auto requested = cached;
    requested.size = {1, 1, 1};
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
    EXPECT_TRUE(RequiresSeparateAliasImage(cached, requested));
    EXPECT_TRUE(RequiresSeparateAliasImage(requested, cached));
}

TEST(AliasingTest, RejectsCrossingRectanglesThatCannotPreserveOneFullBacking) {
    auto wide = MakeImageInfo({128, 32, 1});
    auto tall = wide;
    tall.size = {64, 64, 1};
    EXPECT_FALSE(GetAliasCopyExtent(wide, tall));
    EXPECT_FALSE(GetAliasCopyExtent(tall, wide));
    EXPECT_FALSE(RequiresSeparateAliasImage(wide, tall));
}

TEST(AliasingTest, DoesNotExpandTiledWidthIntersectionSupport) {
    auto backing = MakeImageInfo({128, 32, 1});
    backing.props.is_tiled = true;
    backing.tile_mode = AmdGpu::TileMode::Thin1DThin;
    backing.array_mode = AmdGpu::ArrayMode::Array1DTiledThin1;
    auto writer = backing;
    writer.size = {64, 16, 1};
    EXPECT_FALSE(GetAliasCopyExtent(writer, backing));
    EXPECT_FALSE(RequiresSeparateAliasImage(backing, writer));
}

TEST(AliasingTest, EqualGeometryStillAllowsCompatibleFormatView) {
    const auto cached = MakeImageInfo({128, 32, 1});
    auto requested = cached;
    requested.pixel_format = vk::Format::eR8G8B8A8Srgb;
    EXPECT_TRUE(IsImageCacheMatch(cached, requested, false));
    EXPECT_FALSE(RequiresSeparateAliasImage(cached, requested));
}

TEST(AliasingTest, WidthMergeRejectsExtraMipsLayersAndSamples) {
    const auto backing = MakeImageInfo({128, 32, 1});
    auto writer = backing;
    writer.size = {64, 16, 1};
    writer.resources.levels = 2;
    EXPECT_FALSE(GetAliasCopyExtent(writer, backing));
    writer.resources.levels = 1;
    writer.resources.layers = 2;
    EXPECT_FALSE(GetAliasCopyExtent(writer, backing));
    writer.resources.layers = 1;
    writer.num_samples = 4;
    auto multisampled = backing;
    multisampled.num_samples = 4;
    EXPECT_FALSE(GetAliasCopyExtent(writer, multisampled));
}

TEST(AliasingTest, PartialLinearWriterPreservesUnwrittenRowsAndColumns) {
    const auto backing_info = MakeImageInfo({8, 4, 1});
    auto writer_info = backing_info;
    writer_info.size = {2, 2, 1};
    const auto extent = GetAliasCopyExtent(writer_info, backing_info);
    ASSERT_TRUE(extent);
    std::vector<u32> backing(32);
    for (u32 i = 0; i < backing.size(); ++i) {
        backing[i] = i + 1;
    }
    std::vector<u32> writer(16, 0xDEADBEEF);
    writer[0] = 101;
    writer[1] = 102;
    writer[8] = 103;
    writer[9] = 104;
    for (u32 y = 0; y < extent->height; ++y) {
        std::copy_n(writer.begin() + y * writer_info.pitch, extent->width,
                    backing.begin() + y * backing_info.pitch);
    }
    for (u32 i = 0; i < backing.size(); ++i) {
        const u32 expected = i == 0 ? 101 : i == 1 ? 102 : i == 8 ? 103 : i == 9 ? 104 : i + 1;
        EXPECT_EQ(backing[i], expected) << i;
    }
}

TEST(AliasingTest, PreservesSupportedTiledHeightAliases) {
    auto cached = MakeImageInfo({128, 32, 1});
    cached.props.is_tiled = true;
    cached.tile_mode = AmdGpu::TileMode::Thin1DThin;
    cached.array_mode = AmdGpu::ArrayMode::Array1DTiledThin1;
    auto requested = cached;
    requested.size.height = 16;
    EXPECT_TRUE(RequiresSeparateAliasImage(cached, requested));
}

TEST(ImageCacheMatchTest, RejectsLinearAndTiledImagesWithIdenticalByteSize) {
    ImageInfo cached = MakeImageInfo({128, 128, 1});
    cached.guest_size = 65536;
    ImageInfo requested = cached;
    requested.props.is_tiled = true;
    requested.tile_mode = AmdGpu::TileMode::Thin1DThin;
    requested.array_mode = AmdGpu::ArrayMode::Array1DTiledThin1;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
}

TEST(ImageCacheMatchTest, RejectsChangedPitchSwizzleAndSamples) {
    const ImageInfo cached = MakeImageInfo({128, 128, 1});
    ImageInfo requested = cached;
    ++requested.pitch;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
    requested = cached;
    requested.bank_swizzle = 1;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
    requested = cached;
    requested.alt_tile = true;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
    requested = cached;
    requested.num_samples = 2;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
}

TEST(ImageCacheMatchTest, ExtraMipsDoNotCompensateForMissingArrayLayers) {
    ImageInfo cached = MakeImageInfo({128, 128, 1});
    ImageInfo requested = cached;
    cached.resources = {4, 1};
    requested.resources = {1, 6};
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
}

TEST(ImageCacheMatchTest, PreservesCompatibleFormatViewsAndExactFormatRequests) {
    const ImageInfo cached = MakeImageInfo({128, 128, 1});
    ImageInfo requested = cached;
    requested.pixel_format = vk::Format::eR8G8B8A8Srgb;
    EXPECT_TRUE(IsImageCacheMatch(cached, requested, false));
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, true));
    EXPECT_TRUE(IsImageCacheMatch(cached, cached, true));
}

TEST(ImageCacheMatchTest, PreservesSingleTexelTypeExceptionWithSameMemoryLayout) {
    const ImageInfo cached = MakeImageInfo({1, 1, 1});
    ImageInfo requested = cached;
    requested.type = AmdGpu::ImageType::Color1D;
    EXPECT_TRUE(IsImageCacheMatch(cached, requested, false));
    requested.pitch += 1;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
}

TEST(ImageCacheMatchTest, RejectsDepthAndBlockInterpretationChanges) {
    const ImageInfo cached = MakeImageInfo({128, 128, 1});
    ImageInfo requested = cached;
    requested.props.is_depth = true;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
    requested = cached;
    requested.props.is_block = true;
    EXPECT_FALSE(IsImageCacheMatch(cached, requested, false));
}

} // namespace
} // namespace VideoCore
