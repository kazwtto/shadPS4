// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later
#include "video_core/renderer_vulkan/vk_execution_policy.h"
#include <initializer_list>

using Vulkan::ParseExecutionPolicy;
static_assert(!ParseExecutionPolicy("", "").selected);
static_assert(!ParseExecutionPolicy("typo", "0").selected);
static_assert(ParseExecutionPolicy("", "1").lazy);
static_assert(ParseExecutionPolicy("precise", "0").lazy);
static_assert(!ParseExecutionPolicy("precise", "").native_cache);
static_assert(ParseExecutionPolicy("balanced", "").native_cache);
static_assert(ParseExecutionPolicy("balanced", "").pipeline_workers == 2);
static_assert(ParseExecutionPolicy("fast", "").pipeline_workers == 6);
static_assert(ParseExecutionPolicy("fast", "").module_workers == 2);
static_assert(ParseExecutionPolicy("fast", "0").lazy);
static_assert(ParseExecutionPolicy("precise", "").frames_ahead == 1);
static_assert(ParseExecutionPolicy("balanced", "").frames_ahead == 2);
static_assert(ParseExecutionPolicy("fast", "").frames_ahead == 3);

int main() {
    Vulkan::ConfigureExecutionPolicy("balanced", false);
    if (Vulkan::GetExecutionPolicy().name != "balanced" ||
        !Vulkan::GetExecutionPolicy().lazy) return 2;
    Vulkan::ConfigureExecutionPolicy("precise", false);
    if (Vulkan::GetExecutionPolicy().native_cache ||
        Vulkan::GetExecutionPolicy().frames_ahead != 1) return 3;
    Vulkan::ConfigureExecutionPolicy("fast", false);
    if (Vulkan::GetExecutionPolicy().frames_ahead != 3) return 4;
    for (auto mode : {"precise", "balanced", "fast"}) {
        const auto p = ParseExecutionPolicy(mode, "");
        if (!p.selected || !p.lazy || p.pipeline_workers < 1 || p.pipeline_workers > 6 ||
            p.module_workers < 1 || p.module_workers > 2) {
            return 1;
        }
    }
    return 0;
}
