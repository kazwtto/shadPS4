// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later
#include <gtest/gtest.h>
#include "video_core/renderer_vulkan/effect_probe.h"

TEST(EffectProbe, BoundsReadbacksForLongGameplay) {
    unsigned count{};
    for (unsigned i = 0; i < 100000; ++i) {
        count += Vulkan::EffectProbe::Sample(i);
    }
    EXPECT_EQ(count, 8);
    EXPECT_FALSE(Vulkan::EffectProbe::Sample(0));
    EXPECT_LE(Vulkan::EffectProbe::MaxPending * Vulkan::EffectProbe::MaxImageBytes,
              12 * 1024 * 1024);
}

TEST(EffectProbe, DistinguishesNeutralVectorsFromZeroClearAndHandlesBgra) {
    constexpr std::array<std::uint8_t, 16> bytes{
        127, 128, 0, 255, 0, 0, 0, 255, 255, 32, 127, 0, 128, 127, 20, 1};
    const auto rgba = Vulkan::EffectProbe::Summarize(bytes, false);
    EXPECT_EQ(rgba.pixels, 4);
    EXPECT_EQ(rgba.near_neutral, 2);
    EXPECT_EQ(rgba.zero_rg, 1);
    EXPECT_EQ(rgba.maximum[0], 255);
    const auto bgra = Vulkan::EffectProbe::Summarize(bytes, true);
    EXPECT_EQ(bgra.near_neutral, 0);
    EXPECT_EQ(bgra.zero_rg, 1);
    EXPECT_EQ(bgra.maximum[0], 127);
}

TEST(EffectProbe, EmptyOrPartialPixelCannotBeReadAsAnImage) {
    EXPECT_EQ(Vulkan::EffectProbe::Summarize({}, false).pixels, 0);
    constexpr std::array<std::uint8_t, 3> bytes{128, 128, 128};
    EXPECT_EQ(Vulkan::EffectProbe::Summarize(bytes, false).pixels, 0);
}
