// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <algorithm>
#include <optional>

#include "video_core/texture_cache/aliasing.h"
#include "video_core/texture_cache/host_compatibility.h"
#include "video_core/texture_cache/image_info.h"
#include "video_core/texture_cache/types.h"

namespace VideoCore {

bool HasSameImageMemoryLayout(const ImageInfo& lhs, const ImageInfo& rhs) {
    return lhs.num_bits == rhs.num_bits && lhs.num_samples == rhs.num_samples &&
           lhs.pitch == rhs.pitch && lhs.tile_mode == rhs.tile_mode &&
           lhs.array_mode == rhs.array_mode && lhs.bank_swizzle == rhs.bank_swizzle &&
           lhs.alt_tile == rhs.alt_tile && lhs.props.is_tiled == rhs.props.is_tiled &&
           lhs.props.is_block == rhs.props.is_block && lhs.props.is_pow2 == rhs.props.is_pow2 &&
           lhs.props.is_volume == rhs.props.is_volume && lhs.props.is_depth == rhs.props.is_depth &&
           lhs.props.has_stencil == rhs.props.has_stencil;
}

bool IsImageCacheMatch(const ImageInfo& cached, const ImageInfo& requested, bool exact_format) {
    return cached.guest_address == requested.guest_address &&
           cached.guest_size == requested.guest_size && cached.size == requested.size &&
           HasSameImageMemoryLayout(cached, requested) &&
           IsVulkanFormatCompatible(cached.pixel_format, requested.pixel_format) &&
           (cached.type == requested.type || requested.size == Extent3D{1, 1, 1}) &&
           (!exact_format || cached.pixel_format == requested.pixel_format) &&
           cached.resources.levels >= requested.resources.levels &&
           cached.resources.layers >= requested.resources.layers;
}

std::optional<Extent3D> GetAliasCopyExtent(const ImageInfo& src, const ImageInfo& dst) {
    const bool same_layout = src.guest_address == dst.guest_address &&
                             HasSameImageMemoryLayout(src, dst) && !src.props.is_depth &&
                             !dst.props.is_depth && src.type == dst.type;
    if (!same_layout) {
        return std::nullopt;
    }

    if (src.pixel_format == dst.pixel_format && src.resources.levels == 1 &&
        dst.resources.levels == 1 && src.resources.layers >= dst.resources.layers &&
        src.size.width >= dst.size.width && src.size.height >= dst.size.height &&
        src.size.depth >= dst.size.depth) {
        return dst.size;
    }

    // A pending partial writer must merge into a backing that covers the whole union.
    // Crossing rectangles would lose the writer's uncovered rows/columns when it is committed.
    const bool nested_extents =
        (src.size.width >= dst.size.width && src.size.height >= dst.size.height) ||
        (dst.size.width >= src.size.width && dst.size.height >= src.size.height);
    const bool linear_width_intersection = !src.props.is_tiled && !dst.props.is_tiled &&
                                           src.num_samples == 1 && nested_extents;
    const bool can_copy_intersection = !src.props.is_block && src.resources.levels == 1 &&
                                       dst.resources.levels == 1 && src.resources.layers == 1 &&
                                       dst.resources.layers == 1 && src.size.depth == 1 &&
                                       dst.size.depth == 1 &&
                                       (src.size.width == dst.size.width || linear_width_intersection) &&
                                       IsVulkanFormatCompatible(src.pixel_format, dst.pixel_format);
    if (!can_copy_intersection) {
        return std::nullopt;
    }

    return Extent3D{
        .width = std::min(src.size.width, dst.size.width),
        .height = std::min(src.size.height, dst.size.height),
        .depth = 1,
    };
}

bool RequiresSeparateAliasImage(const ImageInfo& cached, const ImageInfo& requested) {
    return cached.size != requested.size && GetAliasCopyExtent(cached, requested) &&
           GetAliasCopyExtent(requested, cached);
}

} // namespace VideoCore
