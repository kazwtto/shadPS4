// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <array>
#include <gtest/gtest.h>
#include <xxhash.h>

#include "video_core/texture_cache/image_dirty.h"

namespace VideoCore {
namespace {

struct GuestImage {
    std::array<u8, 4096> memory{};
    ImageFlagBits flags = ImageFlagBits::Registered;
    u64 reference{};

    void Mark() { MarkImageMaybeCpuDirty(flags, reference, memory); }
    bool Resolve() { return ResolveImageMaybeCpuDirty(flags, reference, memory); }
};

TEST(TextureDirtyTest, NeighborWriteDoesNotUploadUnchangedImage) {
    GuestImage image;
    image.Mark();
    EXPECT_FALSE(image.Resolve());
    EXPECT_EQ(image.flags, ImageFlagBits::Registered);
}

TEST(TextureDirtyTest, DetectsWriteBeyondFormerTexelPrefix) {
    GuestImage image;
    image.Mark();
    image.memory.back() = 0xAB;
    EXPECT_TRUE(image.Resolve());
    EXPECT_TRUE(True(image.flags & ImageFlagBits::CpuDirty));
    EXPECT_FALSE(True(image.flags & ImageFlagBits::MaybeCpuDirty));
}

TEST(TextureDirtyTest, RepeatedMarksPreserveSnapshotBeforeWrite) {
    GuestImage image;
    image.Mark();
    const u64 original = image.reference;
    image.memory[2048] = 7;
    image.Mark();
    EXPECT_EQ(image.reference, original);
    EXPECT_TRUE(image.Resolve());
}

TEST(TextureDirtyTest, NewEpisodeDoesNotReuseHashBeforeExplicitUpload) {
    GuestImage image;
    image.Mark();
    EXPECT_FALSE(image.Resolve());
    // A definitive fault uploads B without consulting the speculative hash.
    image.memory.fill(1);
    image.flags |= ImageFlagBits::CpuDirty;
    image.flags &= ~ImageFlagBits::Dirty; // Successful upload of B.
    image.Mark();
    image.memory.fill(0); // CPU restores A while the page is unprotected.
    EXPECT_TRUE(image.Resolve());
}

TEST(TextureDirtyTest, ExplicitGpuInvalidationCannotBeSkippedByUnchangedRam) {
    GuestImage image;
    image.Mark();
    image.flags |= ImageFlagBits::GpuDirty;
    EXPECT_FALSE(image.Resolve()); // No speculative resolution; upload is still required.
    EXPECT_TRUE(True(image.flags & ImageFlagBits::GpuDirty));
    EXPECT_TRUE(True(image.flags & ImageFlagBits::Dirty));
}

TEST(TextureDirtyTest, ExplicitCpuInvalidationCannotBeClearedByMatchingHash) {
    GuestImage image;
    image.Mark();
    image.flags |= ImageFlagBits::CpuDirty;
    EXPECT_FALSE(image.Resolve());
    EXPECT_TRUE(True(image.flags & ImageFlagBits::CpuDirty));
}

TEST(TextureDirtyTest, UnchangedGpuModifiedImageRetainsGpuAuthorityFlags) {
    GuestImage image;
    image.flags |= ImageFlagBits::GpuModified | ImageFlagBits::Aliased;
    image.Mark();
    EXPECT_FALSE(image.Resolve());
    EXPECT_EQ(image.flags,
              ImageFlagBits::Registered | ImageFlagBits::GpuModified | ImageFlagBits::Aliased);
}

TEST(TextureDirtyTest, HashValueZeroIsNotAnInitializationSentinel) {
    GuestImage image;
    image.Mark();
    image.reference = 0;
    image.memory.back() = 3;
    image.Mark();
    EXPECT_EQ(image.reference, 0);
    EXPECT_TRUE(image.Resolve());
}

TEST(TextureDirtyTest, SnapshotCoversOnlyTheSpecifiedGuestRange) {
    std::array<u8, 8192> allocation{};
    const std::span<const u8> guest{allocation.data() + 1024, 6000};
    ImageFlagBits flags = ImageFlagBits::Registered;
    u64 reference{};
    MarkImageMaybeCpuDirty(flags, reference, guest);
    allocation.front() = 1; // Not part of this image.
    EXPECT_FALSE(ResolveImageMaybeCpuDirty(flags, reference, guest));
    MarkImageMaybeCpuDirty(flags, reference, guest);
    allocation[7023] = 2; // Last byte in the second page / last mip / padded region.
    EXPECT_TRUE(ResolveImageMaybeCpuDirty(flags, reference, guest));
}

TEST(TextureDirtyTest, TwoPageSnapshotStartsBeforeEitherBoundaryIsUnprotected) {
    constexpr size_t page = 4096;
    EXPECT_TRUE(ImageFitsTwoPages(0x1000, page * 2, page));
    EXPECT_TRUE(ImageFitsTwoPages(0x1F00, page + 256, page));
    EXPECT_FALSE(ImageFitsTwoPages(0x1F00, page + 257, page));
    EXPECT_FALSE(ImageFitsTwoPages(0x1000, page * 2 + 1, page));
    EXPECT_FALSE(ImageFitsTwoPages(0x1000, 0, page));
}

} // namespace
} // namespace VideoCore
