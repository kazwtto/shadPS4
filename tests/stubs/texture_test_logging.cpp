// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <cstdio>
#include <cstdlib>
#include "common/logging/log.h"

// Texture layout tests need no emulator/log sinks. An unexpected format still fails hard.
void assert_fail_impl() {
    std::abort();
}
[[noreturn]] void unreachable_impl() {
    std::abort();
}

namespace Common::Log {
void VLog(Class, Level, const char* file, int line, const char* func, fmt::string_view,
          fmt::format_args) noexcept {
    std::fprintf(stderr, "Unexpected texture test failure: %s:%d %s\n", file, line, func);
}
} // namespace Common::Log
