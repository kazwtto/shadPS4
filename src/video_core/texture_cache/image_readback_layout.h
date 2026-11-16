// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <algorithm>
#include <limits>
#include <optional>

#include "common/types.h"

namespace VideoCore {

/// Buffer-image copies write texels, not the gaps introduced by bufferRowLength.
/// Offsets describe the reserved staging allocation; only the reported spans own data.
class ImageReadbackLayout {
public:
    ImageReadbackLayout() = default;

    [[nodiscard]] static constexpr std::optional<ImageReadbackLayout> Make(
        u64 row_bytes, u64 row_stride, u64 rows) {
        if (row_bytes == 0 || row_stride < row_bytes || rows == 0 ||
            rows > std::numeric_limits<u64>::max() / row_stride) {
            return std::nullopt;
        }
        return ImageReadbackLayout{row_bytes, row_stride, rows};
    }

    [[nodiscard]] static constexpr ImageReadbackLayout Packed(u64 size) {
        return {size, size, size != 0 ? 1ULL : 0ULL};
    }

    [[nodiscard]] constexpr u64 ReservedBytes() const { return row_stride * rows; }
    [[nodiscard]] constexpr u64 PayloadBytes() const { return row_bytes * rows; }
    [[nodiscard]] constexpr bool IsContiguous() const {
        return rows == 1 || row_bytes == row_stride;
    }

    template <typename Callback>
    void ForEachSpan(u64 begin, u64 size, Callback&& callback) const {
        if (size == 0 || begin >= ReservedBytes()) {
            return;
        }
        const u64 end = begin + std::min(size, ReservedBytes() - begin);
        if (IsContiguous()) {
            const u64 data_end = std::min(end, PayloadBytes());
            if (begin < data_end) {
                callback(begin, data_end - begin);
            }
            return;
        }
        for (u64 row = begin / row_stride; row < rows && row * row_stride < end; ++row) {
            const u64 row_begin = row * row_stride;
            const u64 data_begin = std::max(begin, row_begin);
            const u64 data_end = std::min(end, row_begin + row_bytes);
            if (data_begin < data_end) {
                callback(data_begin, data_end - data_begin);
            }
        }
    }

private:
    constexpr ImageReadbackLayout(u64 row_bytes_, u64 row_stride_, u64 rows_)
        : row_bytes{row_bytes_}, row_stride{row_stride_}, rows{rows_} {}

    u64 row_bytes{};
    u64 row_stride{};
    u64 rows{};
};

} // namespace VideoCore
