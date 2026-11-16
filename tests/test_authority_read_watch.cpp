// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <limits>
#include <random>
#include <vector>

#include <gtest/gtest.h>

#include "video_core/authority_read_watch.h"
#include "video_core/buffer_cache/range_set.h"
#include "video_core/gpu_authority_tracker.h"

namespace {
using Range = std::pair<VAddr, size_t>;

std::vector<Range> Gaps(const std::vector<Range>& required, const VideoCore::RangeSet& armed,
                        Range query, bool local) {
    VideoCore::RangeSet protected_ranges;
    for (const auto& [addr, size] : required) {
        if (local) {
            if (auto clipped =
                    VideoCore::ClipReadWatchRange(addr, size, query.first, query.second)) {
                protected_ranges.Add(clipped->first, clipped->second);
            }
        } else {
            protected_ranges.Add(addr, size);
        }
    }
    std::vector<Range> gaps;
    armed.ForEachInRange(query.first, query.second, [&](VAddr begin, VAddr end) {
        protected_ranges.ForEachNotInRange(
            begin, end - begin, [&](VAddr addr, size_t size) { gaps.emplace_back(addr, size); });
    });
    return gaps;
}

TEST(AuthorityReadWatchTest, PreservesDifferentBytesSharingTheSameWatchedPage) {
    const auto query = VideoCore::RoundReadWatchRange(0x1004, 4);
    const auto neighbour = VideoCore::RoundReadWatchRange(0x1ff0, 4);
    EXPECT_EQ(query, neighbour);
    EXPECT_EQ(
        VideoCore::ClipReadWatchRange(neighbour.first, neighbour.second, query.first, query.second),
        std::optional<Range>{query});
}

TEST(AuthorityReadWatchTest, ClipsLongRangesAndRejectsAdjacentAndInvalidRanges) {
    EXPECT_EQ(VideoCore::ClipReadWatchRange(0x1000, 0x5000, 0x2000, 0x2000),
              (std::optional<Range>{{0x2000, 0x2000}}));
    EXPECT_FALSE(VideoCore::ClipReadWatchRange(0x1000, 0x1000, 0x2000, 0x1000));
    EXPECT_FALSE(VideoCore::ClipReadWatchRange(0x1000, 0, 0x1000, 0x1000));
    EXPECT_FALSE(
        VideoCore::ClipReadWatchRange(std::numeric_limits<VAddr>::max() - 2, 4, 0x1000, 0x1000));
}

TEST(AuthorityReadWatchTest, NeverDisarmsUnarmedGapsOrActiveOverlaps) {
    VideoCore::RangeSet armed;
    armed.Add(0x1000, 0x2000);
    armed.Add(0x5000, 0x2000);
    const std::vector<Range> required{{0, 0x2000}, {0x6000, 0x10000}, {0x100000, 0x1000}};
    const Range query{0x1000, 0x6000};
    const std::vector<Range> expected{{0x2000, 0x1000}, {0x5000, 0x1000}};
    EXPECT_EQ(Gaps(required, armed, query, false), expected);
    EXPECT_EQ(Gaps(required, armed, query, true), expected);
}

TEST(AuthorityReadWatchTest, HandlesNoActiveAuthorityAndCompleteCoverage) {
    VideoCore::RangeSet armed;
    armed.Add(0x1000, 0x4000);
    const Range query{0x2000, 0x2000};
    EXPECT_EQ(Gaps({}, armed, query, true), (std::vector<Range>{query}));
    EXPECT_TRUE(Gaps({{0, 0x10000}}, armed, query, true).empty());
}

TEST(AuthorityReadWatchTest, MatchesGlobalRangeSetForRandomSparseQueries) {
    std::mt19937 rng{0x5825};
    for (size_t iteration = 0; iteration < 500; ++iteration) {
        VideoCore::RangeSet armed;
        std::vector<Range> required;
        for (size_t i = 0; i < 24; ++i) {
            const VAddr addr = (rng() % 96) * 4096;
            const size_t size = (1 + rng() % 12) * 4096;
            if (rng() & 1) {
                armed.Add(addr, size);
            } else {
                required.emplace_back(addr, size);
            }
        }
        const Range query{(rng() % 96) * 4096, (1 + rng() % 16) * 4096};
        EXPECT_EQ(Gaps(required, armed, query, true), Gaps(required, armed, query, false));
    }
}

TEST(AuthorityReadWatchTest, OverlapResultsSpillWithoutDroppingOwnership) {
    VideoCore::GpuAuthorityTracker::AuthorityEntries entries;
    std::vector<std::shared_ptr<VideoCore::GpuAuthorityEntry>> owners;
    for (size_t i = 0; i < 24; ++i) {
        auto entry = std::make_shared<VideoCore::GpuAuthorityEntry>();
        entry->guest_begin = i * 4096;
        owners.push_back(entry);
        entries.push_back(std::move(entry));
    }
    ASSERT_EQ(entries.size(), owners.size());
    for (size_t i = 0; i < entries.size(); ++i) {
        EXPECT_EQ(entries[i], owners[i]);
        EXPECT_EQ(entries[i]->guest_begin, i * 4096);
    }
}
} // namespace
