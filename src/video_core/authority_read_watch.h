// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <algorithm>
#include <limits>
#include <optional>
#include <utility>

#include "common/types.h"

namespace VideoCore {

inline std::pair<VAddr, size_t> RoundReadWatchRange(VAddr addr, size_t size) {
    constexpr VAddr page_mask = 4095;
    const VAddr begin = addr & ~page_mask;
    const VAddr end = (addr + size + page_mask) & ~page_mask;
    return {begin, static_cast<size_t>(end - begin)};
}

// Both input ranges are already rounded to read-watch pages. Only this intersection can
// protect pages that RefreshAuthorityReadWatches is considering disarming.
inline std::optional<std::pair<VAddr, size_t>> ClipReadWatchRange(VAddr required_addr,
                                                                  size_t required_size,
                                                                  VAddr watch_addr,
                                                                  size_t watch_size) {
    constexpr auto max = std::numeric_limits<VAddr>::max();
    if (!required_size || !watch_size || required_size > max - required_addr ||
        watch_size > max - watch_addr) {
        return std::nullopt;
    }
    const VAddr begin = std::max(required_addr, watch_addr);
    const VAddr end = std::min(required_addr + required_size, watch_addr + watch_size);
    if (begin >= end) {
        return std::nullopt;
    }
    return std::pair{begin, static_cast<size_t>(end - begin)};
}

} // namespace VideoCore
