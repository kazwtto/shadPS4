// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <algorithm>

#include "common/types.h"

namespace VideoCore {

struct CacheMemoryBudget {
    u64 budget;
    u64 target;

    /// Keep the existing GC floors even when the driver reports a smaller budget. Saturate
    /// subtractions before comparing with a floor: converting a negative spacing to u64
    /// would otherwise put the threshold near UINT64_MAX and effectively disable collection.
    [[nodiscard]] constexpr u64 Threshold(u64 spacing, u64 vacancy_percent, u64 floor) const {
        const u64 threshold = std::min(budget, target);
        const u64 vacancy =
            (threshold / 100) * vacancy_percent + ((threshold % 100) * vacancy_percent) / 100;
        const u64 by_vacancy = budget > vacancy ? budget - vacancy : 0;
        const u64 by_spacing = budget > spacing ? budget - spacing : 0;
        return std::max(floor, std::min(by_vacancy, by_spacing));
    }

    /// Usage below which garbage collection is skipped entirely. Budgets above the target keep
    /// half of the excess. Budgets at or below the target (every integrated GPU and most
    /// mid-range cards) used to get 0, so the collector ran on every submit and evicted images
    /// idle for a few ticks even with plenty of memory free, forcing them to be hashed,
    /// uploaded and detiled again on their next use. Half of the pressure threshold is the
    /// floor: below it there is nothing worth reclaiming, and above it the gentle collection
    /// still runs before pressure is reached.
    [[nodiscard]] constexpr u64 EarlyTrigger(u64 pressure) const {
        const u64 excess = (budget - std::min(budget, target)) / 2;
        return std::max(excess, pressure / 2);
    }
};

} // namespace VideoCore
