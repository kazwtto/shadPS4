// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <span>

namespace Vulkan::EffectProbe {
constexpr std::uint64_t ShaderHash = 0x107d87f9d56574cf;
constexpr std::size_t MaxImageBytes = 4 * 1024 * 1024;
constexpr unsigned MaxPending = 3;

// Eight observations, at most two input images and one output per observation.
constexpr bool Sample(std::uint64_t occurrence) {
    constexpr std::array<std::uint64_t, 8> points{1, 30, 120, 300, 600, 900, 1200, 1800};
    for (auto point : points) {
        if (point == occurrence) {
            return true;
        }
    }
    return false;
}

struct RgSummary {
    std::size_t pixels{};
    std::size_t near_neutral{};
    std::size_t zero_rg{};
    std::array<std::uint8_t, 2> minimum{255, 255};
    std::array<std::uint8_t, 2> maximum{};
};

// Raw byte summary, before interpreting swizzles or sRGB. No claim that every map must be neutral.
inline RgSummary Summarize(std::span<const std::uint8_t> rgba, bool bgra) {
    RgSummary result;
    for (std::size_t i = 0; i + 3 < rgba.size(); i += 4) {
        const auto r = rgba[i + (bgra ? 2 : 0)];
        const auto g = rgba[i + 1];
        ++result.pixels;
        result.near_neutral += r >= 127 && r <= 128 && g >= 127 && g <= 128;
        result.zero_rg += r == 0 && g == 0;
        if (r < result.minimum[0]) result.minimum[0] = r;
        if (g < result.minimum[1]) result.minimum[1] = g;
        if (r > result.maximum[0]) result.maximum[0] = r;
        if (g > result.maximum[1]) result.maximum[1] = g;
    }
    return result;
}
} // namespace Vulkan::EffectProbe
