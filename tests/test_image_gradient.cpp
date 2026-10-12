// SPDX-FileCopyrightText: Copyright 2026 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <bit>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <span>
#include <gtest/gtest.h>

#include "shader_recompiler/backend/spirv/emit_spirv_instructions.h"
#include "shader_recompiler/backend/spirv/spirv_emit_context.h"

namespace Shader::Backend::SPIRV {
namespace {

std::vector<u32> CompileGradient(bool support_min_lod, const IR::Value& clamp,
                                 const IR::Value& offset, const char* name) {
    Profile profile{};
    profile.supported_spirv = 0x00010600;
    profile.supports_image_min_lod = support_min_lod;
    Info info{};
    info.stage = Stage::Compute;
    info.l_stage = LogicalStage::Compute;
    RuntimeInfo runtime{};
    runtime.Initialize(Stage::Compute);
    runtime.cs_info.workgroup_size = {1, 1, 1};
    Bindings bindings{};
    EmitContext ctx{profile, runtime, info, bindings};
    ctx.AddCapability(spv::Capability::Int8);
    ctx.AddCapability(spv::Capability::Int16);
    ctx.AddCapability(spv::Capability::Int64);
    ctx.AddCapability(spv::Capability::Sampled1D);
    const Id image_type =
        ctx.TypeImage(ctx.F32[1], spv::Dim::Dim1D, 0, false, false, 1, spv::ImageFormat::Unknown);
    const Id image = ctx.DefineUniformConst(image_type, 0, bindings.unified++);
    ctx.images.push_back({.data_types = &ctx.F32,
                          .id = image,
                          .sampled_type = ctx.TypeSampledImage(image_type),
                          .image_type = image_type,
                          .view_type = AmdGpu::ImageType::Color1D});
    ctx.sampler_type = ctx.TypeSampler();
    const Id sampler = ctx.DefineUniformConst(ctx.sampler_type, 0, bindings.unified++);
    ctx.samplers.push_back(sampler);
    ctx.interfaces.push_back(image);
    ctx.interfaces.push_back(sampler);
    const Id function_type = ctx.TypeFunction(ctx.void_id);
    const Id function =
        ctx.OpFunction(ctx.void_id, spv::FunctionControlMask::MaskNone, function_type);
    ctx.AddLabel();
    EmitImageGradient(ctx, nullptr, 0, ctx.ConstF32(0.5f), ctx.ConstF32(0.01f), ctx.ConstF32(0.02f),
                      offset, clamp);
    ctx.OpReturn();
    ctx.OpFunctionEnd();
    ctx.AddEntryPoint(spv::ExecutionModel::GLCompute, function, "main", ctx.interfaces);
    ctx.AddExecutionMode(function, spv::ExecutionMode::LocalSize, 1U, 1U, 1U);
    auto code = ctx.Assemble();
    if (const char* directory = std::getenv("SHADPS4_TEST_SPV_DIR")) {
        std::ofstream output{std::filesystem::path{directory} / name, std::ios::binary};
        output.write(reinterpret_cast<const char*>(code.data()), code.size() * sizeof(u32));
    }
    return code;
}

std::span<const u32> FindInstruction(const std::vector<u32>& code, spv::Op opcode) {
    for (size_t offset = 5; offset < code.size();) {
        const u32 count = code[offset] >> 16;
        if (count == 0 || offset + count > code.size()) {
            return {};
        }
        if ((code[offset] & 0xffff) == static_cast<u32>(opcode)) {
            return std::span{code}.subspan(offset, count);
        }
        offset += count;
    }
    return {};
}

bool HasMinLodCapability(const std::vector<u32>& code) {
    for (size_t offset = 5; offset < code.size(); offset += code[offset] >> 16) {
        if ((code[offset] & 0xffff) == static_cast<u32>(spv::Op::OpCapability) &&
            code[offset + 1] == static_cast<u32>(spv::Capability::MinLod)) {
            return true;
        }
    }
    return false;
}

TEST(ImageGradientTest, EmitsClampAfterGradientsAndConstantOffset) {
    const auto code = CompileGradient(true, IR::Value{2.0f}, IR::Value{u32{0}}, "clamped.spv");
    const auto instruction = FindInstruction(code, spv::Op::OpImageSampleExplicitLod);
    ASSERT_EQ(instruction.size(), 10);
    EXPECT_EQ(instruction[5], static_cast<u32>(spv::ImageOperandsMask::Grad) |
                                  static_cast<u32>(spv::ImageOperandsMask::ConstOffset) |
                                  static_cast<u32>(spv::ImageOperandsMask::MinLod));
    EXPECT_TRUE(HasMinLodCapability(code));
    // Resolve the final operand to its constant, rather than accepting an arbitrary valid ID.
    bool found_clamp = false;
    for (size_t offset = 5; offset < code.size(); offset += code[offset] >> 16) {
        if ((code[offset] & 0xffff) == static_cast<u32>(spv::Op::OpConstant) &&
            code[offset + 2] == instruction[9]) {
            found_clamp = code[offset + 3] == std::bit_cast<u32>(2.0f);
        }
    }
    EXPECT_TRUE(found_clamp);
}

TEST(ImageGradientTest, DoesNotRequireMinLodWithoutClamp) {
    const auto code = CompileGradient(true, {}, {}, "unclamped.spv");
    const auto instruction = FindInstruction(code, spv::Op::OpImageSampleExplicitLod);
    ASSERT_EQ(instruction.size(), 8);
    EXPECT_EQ(instruction[5], static_cast<u32>(spv::ImageOperandsMask::Grad));
    EXPECT_FALSE(HasMinLodCapability(code));
}

TEST(ImageGradientTest, PreservesFallbackOnUnsupportedDevice) {
    const auto code = CompileGradient(false, IR::Value{2.0f}, {}, "unsupported.spv");
    const auto instruction = FindInstruction(code, spv::Op::OpImageSampleExplicitLod);
    ASSERT_EQ(instruction.size(), 8);
    EXPECT_EQ(instruction[5], static_cast<u32>(spv::ImageOperandsMask::Grad));
    EXPECT_FALSE(HasMinLodCapability(code));
}

} // namespace
} // namespace Shader::Backend::SPIRV
