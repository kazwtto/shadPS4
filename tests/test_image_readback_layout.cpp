// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <algorithm>
#include <array>
#include <limits>
#include <random>
#include <vector>

#include <gtest/gtest.h>

#include "video_core/texture_cache/image_readback_layout.h"

namespace VideoCore {
namespace {

void CopyValid(const ImageReadbackLayout& layout, std::vector<u8>& guest,
               const std::vector<u8>& staging, u64 begin, u64 size) {
    layout.ForEachSpan(begin, size, [&](u64 offset, u64 length) {
        std::copy_n(staging.begin() + offset, length, guest.begin() + offset);
    });
}

TEST(ImageReadbackLayout, ExposureUpdatesFourBytesAndPreserves508PaddingBytes) {
    const auto layout = ImageReadbackLayout::Make(4, 512, 1);
    ASSERT_TRUE(layout);
    std::vector<u8> guest(512, 0xA5);
    std::vector<u8> staging(512, 0xCC);
    const std::array<u8, 4> half_floats{0x00, 0x3C, 0x00, 0x40};
    std::copy(half_floats.begin(), half_floats.end(), staging.begin());
    CopyValid(*layout, guest, staging, 0, 512);
    EXPECT_TRUE(std::equal(half_floats.begin(), half_floats.end(), guest.begin()));
    EXPECT_TRUE(std::all_of(guest.begin() + 4, guest.end(), [](u8 b) { return b == 0xA5; }));
    EXPECT_EQ(layout->ReservedBytes(), 512);
    EXPECT_EQ(layout->PayloadBytes(), 4);
}

TEST(ImageReadbackLayout, PaddingChangesAfterSchedulingAreNotOverwritten) {
    const auto layout = *ImageReadbackLayout::Make(4, 512, 1);
    std::vector<u8> guest(512, 0x11);
    std::vector<u8> staging(512, 0x22);
    std::fill(guest.begin() + 4, guest.end(), 0x33);
    CopyValid(layout, guest, staging, 0, 512);
    EXPECT_TRUE(std::all_of(guest.begin(), guest.begin() + 4, [](u8 b) { return b == 0x22; }));
    EXPECT_TRUE(std::all_of(guest.begin() + 4, guest.end(), [](u8 b) { return b == 0x33; }));
}

TEST(ImageReadbackLayout, PreservesPaddingAcrossRowsSlicesAndLayers) {
    const auto layout = *ImageReadbackLayout::Make(12, 32, 3 * 2 * 2);
    std::vector<u8> guest(384, 0xA5);
    std::vector<u8> staging(384, 0xCC);
    for (size_t row = 0; row < 12; ++row) {
        std::fill_n(staging.begin() + row * 32, 12, static_cast<u8>(row));
    }
    CopyValid(layout, guest, staging, 0, guest.size());
    for (size_t i = 0; i < guest.size(); ++i) {
        EXPECT_EQ(guest[i], i % 32 < 12 ? static_cast<u8>(i / 32) : 0xA5) << i;
    }
    EXPECT_FALSE(layout.IsContiguous());
}

TEST(ImageReadbackLayout, ClipsPageCompletionWithoutTouchingOtherBytes) {
    const auto layout = *ImageReadbackLayout::Make(8, 16, 4);
    std::vector<u8> guest(64, 0xA5);
    std::vector<u8> staging(64, 0x22);
    CopyValid(layout, guest, staging, 6, 32);
    for (size_t i = 0; i < guest.size(); ++i) {
        const bool copied = (i >= 6 && i < 8) || (i >= 16 && i < 24) ||
                            (i >= 32 && i < 38);
        EXPECT_EQ(guest[i], copied ? 0x22 : 0xA5) << i;
    }
}

TEST(ImageReadbackLayout, CoalescesDenseRowsIntoOneSpan) {
    const auto layout = *ImageReadbackLayout::Make(8, 8, 100);
    u32 calls{};
    layout.ForEachSpan(3, 1000, [&](u64 offset, u64 size) {
        ++calls;
        EXPECT_EQ(offset, 3);
        EXPECT_EQ(size, 797);
    });
    EXPECT_EQ(calls, 1);
    EXPECT_TRUE(layout.IsContiguous());
}

TEST(ImageReadbackLayout, RejectsInvalidAndOverflowingAllocations) {
    EXPECT_FALSE(ImageReadbackLayout::Make(0, 8, 1));
    EXPECT_FALSE(ImageReadbackLayout::Make(4, 0, 1));
    EXPECT_FALSE(ImageReadbackLayout::Make(9, 8, 1));
    EXPECT_FALSE(ImageReadbackLayout::Make(4, 8, 0));
    EXPECT_FALSE(ImageReadbackLayout::Make(4, 8, std::numeric_limits<u64>::max()));
}

TEST(ImageReadbackLayout, HugeQueriesCannotWrapOrExposePadding) {
    const auto layout = *ImageReadbackLayout::Make(4, 512, 1);
    u64 copied{};
    layout.ForEachSpan(2, std::numeric_limits<u64>::max(), [&](u64 offset, u64 size) {
        EXPECT_EQ(offset, 2);
        copied += size;
    });
    EXPECT_EQ(copied, 2);
    layout.ForEachSpan(std::numeric_limits<u64>::max(), 10, [&](u64, u64) { FAIL(); });
    layout.ForEachSpan(4, 508, [&](u64, u64) { FAIL(); });
}

TEST(ImageReadbackLayout, AuthorityPrefixIsPayloadRatherThanReservation) {
    const auto exposure = *ImageReadbackLayout::Make(4, 512, 1);
    EXPECT_TRUE(exposure.IsContiguous());
    EXPECT_EQ(exposure.PayloadBytes(), 4);
    EXPECT_FALSE(ImageReadbackLayout::Make(4, 512, 2)->IsContiguous());
    const auto dense = *ImageReadbackLayout::Make(32, 32, 16);
    EXPECT_TRUE(dense.IsContiguous());
    EXPECT_EQ(dense.PayloadBytes(), 512);
}

TEST(ImageReadbackLayout, EmptyLayoutNeverEmitsData) {
    ImageReadbackLayout layout;
    EXPECT_EQ(layout.ReservedBytes(), 0);
    EXPECT_EQ(layout.PayloadBytes(), 0);
    layout.ForEachSpan(0, 100, [&](u64, u64) { FAIL(); });
}

TEST(ImageReadbackLayout, RandomQueriesMatchIndependentByteCoverage) {
    std::mt19937 random{0x5825};
    for (u32 trial = 0; trial < 200; ++trial) {
        const u64 stride = 1 + random() % 64;
        const u64 row_bytes = 1 + random() % stride;
        const u64 rows = 1 + random() % 8;
        const u64 total = stride * rows;
        const auto layout = *ImageReadbackLayout::Make(row_bytes, stride, rows);
        std::vector<u8> guest(total, 0xA5);
        std::vector<u8> staging(total, 0x22);
        const u64 begin = random() % (total + 10);
        const u64 size = random() % (total + 10);
        CopyValid(layout, guest, staging, begin, size);
        for (u64 i = 0; i < total; ++i) {
            const bool copied = i >= begin && i - begin < size && i % stride < row_bytes;
            ASSERT_EQ(guest[i], copied ? 0x22 : 0xA5) << "trial=" << trial << " byte=" << i;
        }
    }
}

} // namespace
} // namespace VideoCore
