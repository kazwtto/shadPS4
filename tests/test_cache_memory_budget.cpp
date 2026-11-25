// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <limits>

#include <gtest/gtest.h>

#include "video_core/cache_memory_budget.h"

namespace {

using VideoCore::CacheMemoryBudget;

// Mirrors the texture cache: pressure uses a 1 GiB spacing, 60% vacancy and a 1.5 GiB floor.
constexpr u64 TexturePressure(const CacheMemoryBudget& budget) {
    return budget.Threshold(1_GB, 60, 1_GB + 512_MB);
}

TEST(CacheMemoryBudget, LowBudgetsKeepGcFloorsInsteadOfWrappingToHugeValues) {
    for (const u64 bytes : {0ULL, 256_MB, 512_MB - 1, 512_MB, 1_GB - 1, 1_GB}) {
        SCOPED_TRACE(bytes);
        const CacheMemoryBudget budget{bytes, 8_GB};
        EXPECT_EQ(budget.Threshold(1_GB, 60, 1_GB), 1_GB);
        EXPECT_EQ(budget.Threshold(512_MB, 20, 2_GB), 2_GB);
        EXPECT_EQ(budget.Threshold(1_GB, 60, 1_GB + 512_MB), 1_GB + 512_MB);
        EXPECT_EQ(budget.Threshold(512_MB, 20, 3_GB), 3_GB);
        EXPECT_EQ(budget.EarlyTrigger(TexturePressure(budget)), 768_MB);
    }
}

TEST(CacheMemoryBudget, PreservesExistingThresholdsForTypicalBudgets) {
    const CacheMemoryBudget two_gib{2_GB, 8_GB};
    EXPECT_EQ(two_gib.Threshold(1_GB, 60, 1_GB), 1_GB);
    EXPECT_EQ(two_gib.Threshold(512_MB, 20, 2_GB), 2_GB);
    EXPECT_EQ(two_gib.EarlyTrigger(TexturePressure(two_gib)), 768_MB);

    const CacheMemoryBudget eight_gib{8_GB, 8_GB};
    EXPECT_EQ(eight_gib.Threshold(1_GB, 60, 1_GB), 3'435'973'837ULL);
    EXPECT_EQ(eight_gib.Threshold(512_MB, 20, 2_GB), 6'871'947'674ULL);
    EXPECT_EQ(eight_gib.EarlyTrigger(TexturePressure(eight_gib)), 1'717'986'918ULL);

    const CacheMemoryBudget sixteen_gib{16_GB, 8_GB};
    EXPECT_EQ(sixteen_gib.Threshold(1_GB, 60, 1_GB), 12'025'908'429ULL);
    EXPECT_EQ(sixteen_gib.Threshold(512_MB, 20, 3_GB), 15'461'882'266ULL);
    EXPECT_EQ(sixteen_gib.EarlyTrigger(TexturePressure(sixteen_gib)), 6'012'954'214ULL);
}

TEST(CacheMemoryBudget, EarlyTriggerNoLongerCollectsOnEverySubmitAtOrBelowTarget) {
    // A 4 GiB integrated GPU budget: GC used to run with any usage at all.
    const CacheMemoryBudget four_gib{4_GB, 8_GB};
    const u64 pressure = TexturePressure(four_gib);
    EXPECT_EQ(pressure, 1'717'986'919ULL);
    EXPECT_EQ(four_gib.EarlyTrigger(pressure), 858'993'459ULL);
}

TEST(CacheMemoryBudget, EarlyTriggerKeepsGentleCollectionReachable) {
    for (const u64 bytes : {0ULL, 512_MB, 1_GB, 2_GB, 3_GB, 4_GB, 6_GB, 8_GB, 8_GB + 1, 12_GB,
                            16_GB, 24_GB, 64_GB}) {
        SCOPED_TRACE(bytes);
        const CacheMemoryBudget budget{bytes, 8_GB};
        const u64 pressure = TexturePressure(budget);
        const u64 trigger = budget.EarlyTrigger(pressure);
        EXPECT_GT(trigger, 0u);
        EXPECT_LT(trigger, pressure);
        // Never collects earlier than the previous rule did.
        EXPECT_GE(trigger, (bytes - std::min(bytes, 8_GB)) / 2);
    }
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
    static_assert(budget.EarlyTrigger(maximum) == maximum / 2);
    EXPECT_EQ(budget.Threshold(0, 0, 0), maximum);
    EXPECT_EQ(budget.Threshold(0, 100, 0), 0u);
    EXPECT_EQ(budget.EarlyTrigger(0), 0u);
    EXPECT_EQ(budget.EarlyTrigger(maximum), maximum / 2);
    EXPECT_EQ((CacheMemoryBudget{maximum, 8_GB}.EarlyTrigger(0)), (maximum - 8_GB) / 2);
}

} // namespace
