// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later
#include <array>
#include <gtest/gtest.h>
#include "video_core/texture_cache/mip_upload_baseline.h"

namespace VideoCore {
TEST(MipUploadBaseline, FirstCpuUploadProtectsLaterGpuContent) {
    MipUploadBaseline baseline;
    std::array<int, 2> ram{4, 5}, gpu = ram;
    baseline.Record(0, 123);
    gpu[0] = 99;
    if (!baseline.Preserve(0, 123, true, false)) {
        gpu = ram;
    }
    EXPECT_EQ(gpu[0], 99);
}
TEST(MipUploadBaseline, RealCpuChangeAllowsUploadAndNewReference) {
    MipUploadBaseline baseline;
    baseline.Record(0, 123);
    EXPECT_FALSE(baseline.Preserve(0, 456, true, false));
    baseline.Record(0, 456);
    EXPECT_TRUE(baseline.Preserve(0, 456, true, false));
    EXPECT_FALSE(baseline.Preserve(0, 123, true, false));
}
TEST(MipUploadBaseline, ZeroRequiresExplicitValidReference) {
    MipUploadBaseline baseline;
    EXPECT_FALSE(baseline.Preserve(0, 0, true, false));
    baseline.Record(0, 0);
    EXPECT_TRUE(baseline.Preserve(0, 0, true, false));
}
TEST(MipUploadBaseline, GpuBufferWriteMustOverrideMatchingRamReference) {
    MipUploadBaseline baseline;
    baseline.Record(0, 123);
    EXPECT_FALSE(baseline.Preserve(0, 123, true, true));
    EXPECT_FALSE(baseline.Preserve(0, 123, false, false));
}
TEST(MipUploadBaseline, MipsRemainIndependentAndBoundsAreChecked) {
    MipUploadBaseline baseline;
    baseline.Record(15, 8);
    EXPECT_TRUE(baseline.Preserve(15, 8, true, false));
    EXPECT_FALSE(baseline.Preserve(0, 8, true, false));
    baseline.Record(16, 1);
    EXPECT_FALSE(baseline.Preserve(16, 1, true, false));
}
} // namespace VideoCore
