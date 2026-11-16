// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <cstdlib>
#include <string_view>

namespace Vulkan {
// These are compilation/cache policies, never shader accuracy or barrier shortcuts.
struct ExecutionPolicy {
    bool selected{};
    bool lazy{};
    bool native_cache{true};
    unsigned pipeline_workers{6};
    unsigned module_workers{2};
    std::string_view name{"legacy"};
    unsigned frames_ahead{2};
};

constexpr ExecutionPolicy ParseExecutionPolicy(std::string_view mode, std::string_view lazy) {
    ExecutionPolicy result{};
    if (mode == "precise") {
        result = {true, true, false, 1, 1, "precise", 1};
    } else if (mode == "balanced") {
        result = {true, true, true, 2, 1, "balanced", 2};
    } else if (mode == "fast") {
        result = {true, true, true, 6, 2, "fast", 3};
    }
    // Lazy can also be selected alone. Profiles always use it: do not silently
    // reintroduce serialized guest states through an environment override.
    if (lazy == "1") {
        result.selected = true;
        result.lazy = true;
    }
    return result;
}

inline ExecutionPolicy& MutableExecutionPolicy() {
    static auto policy = [] {
        const char* mode = std::getenv("SHADPS4_GPU_MODE");
        const char* lazy = std::getenv("SHADPS4_LAZY_CACHE");
        return ParseExecutionPolicy(mode ? mode : "", lazy ? lazy : "");
    }();
    return policy;
}

inline const ExecutionPolicy& GetExecutionPolicy() {
    return MutableExecutionPolicy();
}

// Call during CLI parsing, before any renderer or worker is created.
inline void ConfigureExecutionPolicy(std::string_view mode, bool lazy) {
    const auto& inherited = GetExecutionPolicy();
    MutableExecutionPolicy() = ParseExecutionPolicy(
        mode.empty() ? inherited.name : mode, (lazy || inherited.lazy) ? "1" : "");
}
} // namespace Vulkan
