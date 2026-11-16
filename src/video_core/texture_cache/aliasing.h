// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <optional>

#include "video_core/texture_cache/types.h"

namespace VideoCore {

struct ImageInfo;

/// Descriptors with the same dimensions/address can still interpret guest bytes differently.
[[nodiscard]] bool HasSameImageMemoryLayout(const ImageInfo& lhs, const ImageInfo& rhs);

/// Checks resource coverage independently for mip levels and array layers.
[[nodiscard]] bool IsImageCacheMatch(const ImageInfo& cached, const ImageInfo& requested,
                                     bool exact_format);

[[nodiscard]] std::optional<Extent3D> GetAliasCopyExtent(const ImageInfo& src,
                                                         const ImageInfo& dst);

/// A view cannot change mip-zero geometry. Only split when both copy directions are supported.
[[nodiscard]] bool RequiresSeparateAliasImage(const ImageInfo& cached, const ImageInfo& requested);

} // namespace VideoCore
