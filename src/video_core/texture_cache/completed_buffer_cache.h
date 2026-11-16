// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#pragma once

#include <array>
#include <memory>
#include <mutex>
#include <utility>

#include "common/types.h"

namespace VideoCore {

/// Owns only completed scratch allocations. A checked-out entry must stay with the
/// scheduler until its last GPU use completes; Return must never run at CPU frame end.
template <typename Resource>
class CompletedBufferCache {
public:
    struct Entry {
        std::unique_ptr<Resource> resource;
        u64 capacity{};
        u64 allocation_bytes{};
    };

    explicit CompletedBufferCache(u64 max_bytes) : max_bytes{max_bytes} {}

    [[nodiscard]] Entry Take(u64 size) {
        std::scoped_lock lock{mutex};
        if (closed || size == 0) {
            return {};
        }
        size_t best = entries.size();
        for (size_t i = 0; i < entries.size(); ++i) {
            const auto& entry = entries[i];
            // Do not consume a large allocation for a tiny request. Use a difference to
            // compare against twice the request without overflowing the size argument.
            if (entry.resource && entry.capacity >= size && entry.capacity - size <= size &&
                (best == entries.size() || entry.capacity < entries[best].capacity)) {
                best = i;
            }
        }
        if (best == entries.size()) {
            return {};
        }
        retained_bytes -= entries[best].allocation_bytes;
        return std::exchange(entries[best], {});
    }

    void Return(Entry entry) {
        if (!entry.resource || entry.capacity == 0 || entry.allocation_bytes == 0 ||
            entry.allocation_bytes > max_bytes) {
            return;
        }
        std::scoped_lock lock{mutex};
        if (closed || entry.allocation_bytes > max_bytes - retained_bytes) {
            return;
        }
        for (auto& available : entries) {
            if (!available.resource) {
                retained_bytes += entry.allocation_bytes;
                available = std::move(entry);
                return;
            }
        }
        // A rejected entry is destroyed after the lock, when the parameter goes out of scope.
    }

    /// Stop retaining allocations at owner shutdown. Deferred returns can still safely
    /// hold the cache alive, but destroy their resources when the GPU releases them.
    void Close() {
        std::array<Entry, MaxEntries> released;
        {
            std::scoped_lock lock{mutex};
            closed = true;
            retained_bytes = 0;
            released.swap(entries);
        }
    }

private:
    static constexpr size_t MaxEntries = 16;
    const u64 max_bytes;
    std::mutex mutex;
    std::array<Entry, MaxEntries> entries;
    u64 retained_bytes{};
    bool closed{};
};

} // namespace VideoCore
