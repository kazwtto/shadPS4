// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <cstdlib>

namespace VideoCore::Gow3Diagnostics {

// Read once per process. These controls leave persisted game settings untouched.
inline bool EnvironmentEnabled(const char* name) {
    const char* value = std::getenv(name);
    return value && value[0] == '1' && value[1] == '\0';
}

inline bool Enabled() {
    static const bool enabled = EnvironmentEnabled("SHADPS4_GOW3_DIAGNOSTICS");
    return enabled;
}

inline bool ConservativeAuthority() {
    static const bool enabled = EnvironmentEnabled("SHADPS4_GOW3_CONSERVATIVE_AUTHORITY");
    return enabled;
}

inline bool DisableImageCopy() {
    static const bool enabled = EnvironmentEnabled("SHADPS4_GOW3_NO_IMAGE_COPY_HLE");
    return enabled;
}

inline bool DisableImageClear() {
    static const bool enabled = EnvironmentEnabled("SHADPS4_GOW3_NO_IMAGE_CLEAR_HLE");
    return enabled;
}

inline bool DisableMetaClear() {
    static const bool enabled = EnvironmentEnabled("SHADPS4_GOW3_NO_META_CLEAR_HLE");
    return enabled;
}

inline bool WaitForShaders() {
    static const bool enabled = EnvironmentEnabled("SHADPS4_WAIT_FOR_SHADERS");
    return enabled;
}

inline bool DisablePostFx() {
    static const bool enabled = EnvironmentEnabled("SHADPS4_DISABLE_POSTFX");
    return enabled;
}

} // namespace VideoCore::Gow3Diagnostics
