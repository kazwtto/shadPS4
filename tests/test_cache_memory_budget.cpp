// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <limits>

#include <gtest/gtest.h>

#include "video_core/cache_memory_budget.h"

namespace {

using VideoCore::CacheMemoryBudget;

TEST(CacheMemoryBudget, LowBudgetsKeepGcFloorsInsteadOfWrappingToHugeValues) {
    for (const u64 bytes : {0ULL, 256_MB, 512_MB - 1, 512_MB, 1_GB - 1, 1_GB}) {
        SCOPED_TRACE(bytes);
        const CacheMemoryBudget budget{bytes, 8_GB};
        EXPECT_EQ(budget.Threshold(1_GB, 60, 1_GB), 1_GB);
        EXPECT_EQ(budget.Threshold(512_MB, 20, 2_GB), 2_GB);
        EXPECT_EQ(budget.Threshold(1_GB, 60, 1_GB + 512_MB), 1_GB + 512_MB);
        EXPECT_EQ(budget.Threshold(512_MB, 20, 3_GB), 3_GB);
        EXPECT_EQ(budget.EarlyTrigger(), 0u);
    }
}

TEST(CacheMemoryBudget, PreservesExistingThresholdsForTypicalBudgets) {
    const CacheMemoryBudget two_gib{2_GB, 8_GB};
    EXPECT_EQ(two_gib.Threshold(1_GB, 60, 1_GB), 1_GB);
    EXPECT_EQ(two_gib.Threshold(512_MB, 20, 2_GB), 2_GB);

    const CacheMemoryBudget eight_gib{8_GB, 8_GB};
    EXPECT_EQ(eight_gib.Threshold(1_GB, 60, 1_GB), 3'435'973'837ULL);
    EXPECT_EQ(eight_gib.Threshold(512_MB, 20, 2_GB), 6'871'947'674ULL);
    EXPECT_EQ(eight_gib.EarlyTrigger(), 0u);

    const CacheMemoryBudget sixteen_gib{16_GB, 8_GB};
    EXPECT_EQ(sixteen_gib.Threshold(1_GB, 60, 1_GB), 12'025'908'429ULL);
    EXPECT_EQ(sixteen_gib.Threshold(512_MB, 20, 3_GB), 15'461'882'266ULL);
    EXPECT_EQ(sixteen_gib.EarlyTrigger(), 4_GB);
}

TEST(CacheMemoryBudget, SpacingDoesNotUnderflowWhenItExceedsTheBudget) {
    const CacheMemoryBudget budget{128_MB, 8_GB};
    EXPECT_EQ(budget.Threshold(1_GB, 60, 0), 0u);
    EXPECT_EQ(budget.Threshold(512_MB, 20, 0), 0u);
}

TEST(CacheMemoryBudget, PercentageAndEarlyTriggerDoNotOverflowAtUint64Max) {
    constexpr u64 maximum = std::numeric_limits<u64>::max();
    constexpr CacheMemoryBudget budget{maximum, maximum};
    static_assert(budget.Threshold(0, 100, 0) == 0);
    EXPECT_EQ(budget.Threshold(0, 0, 0), maximum);
    EXPECT_EQ(budget.Threshold(0, 100, 0), 0u);
    EXPECT_EQ(budget.EarlyTrigger(), 0u);
    EXPECT_EQ((CacheMemoryBudget{maximum, 8_GB}.EarlyTrigger()), (maximum - 8_GB) / 2);
}

} // namespace
