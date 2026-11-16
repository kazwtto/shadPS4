// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <atomic>
#include <future>
#include <memory>
#include <thread>
#include <vector>

#include <gtest/gtest.h>

#include "video_core/texture_cache/completed_buffer_cache.h"

namespace {

struct Allocation {
    Allocation(int id, std::atomic<int>* destroyed) : id{id}, destroyed{destroyed} {}
    ~Allocation() { ++*destroyed; }
    int id;
    std::atomic<int>* destroyed;
};

using Cache = VideoCore::CompletedBufferCache<Allocation>;

Cache::Entry MakeEntry(std::atomic<int>& destroyed, int id, u64 size, u64 allocation_bytes = 0) {
    return {.resource = std::make_unique<Allocation>(id, &destroyed),
            .capacity = size,
            .allocation_bytes = allocation_bytes ? allocation_bytes : size};
}

TEST(CompletedBufferCache, CheckedOutBuffersCannotBeReusedUntilReturned) {
    std::atomic<int> destroyed{};
    Cache cache{32_MB};
    cache.Return(MakeEntry(destroyed, 1, 1_MB));
    auto in_flight = cache.Take(1_MB);
    ASSERT_NE(in_flight.resource, nullptr);
    EXPECT_EQ(in_flight.resource->id, 1);
    EXPECT_EQ(cache.Take(1_MB).resource, nullptr);
    EXPECT_EQ(destroyed, 0);
    cache.Return(std::move(in_flight));
    auto completed = cache.Take(1_MB);
    ASSERT_NE(completed.resource, nullptr);
    EXPECT_EQ(completed.resource->id, 1);
    cache.Return(std::move(completed));
    cache.Close();
    EXPECT_EQ(destroyed, 1);
}

TEST(CompletedBufferCache, ChoosesSmallestFittingAllocationAndRejectsExcessiveSlack) {
    std::atomic<int> destroyed{};
    Cache cache{32_MB};
    cache.Return(MakeEntry(destroyed, 8, 8_MB));
    cache.Return(MakeEntry(destroyed, 4, 4_MB));
    cache.Return(MakeEntry(destroyed, 2, 2_MB));
    auto best = cache.Take(3_MB);
    ASSERT_NE(best.resource, nullptr);
    EXPECT_EQ(best.resource->id, 4);
    EXPECT_EQ(cache.Take(512_KB).resource, nullptr);
    EXPECT_EQ(cache.Take(16_MB).resource, nullptr);
    EXPECT_EQ(cache.Take(0).resource, nullptr);
}

TEST(CompletedBufferCache, LimitsActualAllocationBytesInsteadOfOnlyRequestedCapacity) {
    std::atomic<int> destroyed{};
    Cache cache{3_MB};
    cache.Return(MakeEntry(destroyed, 1, 1_MB, 2_MB));
    cache.Return(MakeEntry(destroyed, 2, 1_MB, 2_MB));
    EXPECT_EQ(destroyed, 1);
    auto retained = cache.Take(1_MB);
    ASSERT_NE(retained.resource, nullptr);
    EXPECT_EQ(retained.resource->id, 1);
    cache.Return(MakeEntry(destroyed, 3, 1_MB, 2_MB));
    cache.Return(std::move(retained));
    EXPECT_EQ(destroyed, 2);
    cache.Close();
    EXPECT_EQ(destroyed, 3);
}

TEST(CompletedBufferCache, BoundsEntryCountAndDestroysOversizedOrDisabledReturns) {
    std::atomic<int> destroyed{};
    Cache cache{32_MB};
    for (int i = 0; i < 20; ++i) {
        cache.Return(MakeEntry(destroyed, i, 1_KB));
    }
    EXPECT_EQ(destroyed, 4);
    cache.Return(MakeEntry(destroyed, 20, 64_MB));
    EXPECT_EQ(destroyed, 5);
    cache.Close();
    EXPECT_EQ(destroyed, 21);

    Cache disabled{0};
    disabled.Return(MakeEntry(destroyed, 21, 1_KB));
    EXPECT_EQ(destroyed, 22);
    EXPECT_EQ(disabled.Take(1_KB).resource, nullptr);
}

TEST(CompletedBufferCache, DeferredReturnOutlivesOwnerAndReleasesAfterClose) {
    std::atomic<int> destroyed{};
    auto owner = std::make_shared<Cache>(32_MB);
    const std::weak_ptr<Cache> weak = owner;
    std::packaged_task<void()> completion{
        [cache = owner, entry = MakeEntry(destroyed, 1, 1_MB)]() mutable {
            cache->Return(std::move(entry));
        }};
    owner->Close();
    owner.reset();
    EXPECT_FALSE(weak.expired());
    EXPECT_EQ(destroyed, 0);
    completion();
    EXPECT_EQ(destroyed, 1);
    completion = {};
    EXPECT_TRUE(weak.expired());
}

TEST(CompletedBufferCache, ConcurrentReturnsAndAcquisitionsKeepUniqueOwnership) {
    std::atomic<int> created{};
    std::atomic<int> destroyed{};
    Cache cache{32_MB};
    {
        std::vector<std::jthread> workers;
        for (int i = 0; i < 4; ++i) {
            workers.emplace_back([&] {
                for (int j = 0; j < 1000; ++j) {
                    auto entry = cache.Take(1_KB);
                    if (!entry.resource) {
                        entry = MakeEntry(destroyed, created.fetch_add(1), 1_KB);
                    }
                    cache.Return(std::move(entry));
                }
            });
        }
    }
    cache.Close();
    EXPECT_EQ(destroyed.load(), created.load());
    EXPECT_EQ(cache.Take(1_KB).resource, nullptr);
    cache.Return(MakeEntry(destroyed, created.fetch_add(1), 1_KB));
    EXPECT_EQ(destroyed.load(), created.load());
}

} // namespace
