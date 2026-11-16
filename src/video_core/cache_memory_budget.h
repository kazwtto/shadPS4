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

    [[nodiscard]] constexpr u64 EarlyTrigger() const {
        return (budget - std::min(budget, target)) / 2;
    }
};

} // namespace VideoCore
