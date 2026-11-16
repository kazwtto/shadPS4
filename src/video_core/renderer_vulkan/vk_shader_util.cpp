// SPDX-FileCopyrightText: Copyright 2024 shadPS4 Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <array>
#include <mutex>
#include <shared_mutex>
#include <type_traits>
#include <unordered_set>

#include <glslang/Include/ResourceLimits.h>
#include <glslang/Public/ShaderLang.h>
#include <glslang/SPIRV/GlslangToSpv.h>
#include <glslang/build_info.h>

#include "common/assert.h"
#include "common/logging/log.h"
#include "common/path_util.h"
#include "common/sha1.h"
#include "core/emulator_settings.h"
#include "shader_recompiler/backend/spirv/raw_access_chains.h"
#include "video_core/host_shader_cache.h"
#include "video_core/renderer_vulkan/vk_shader_util.h"

namespace Vulkan {

namespace {
// Increment when compiler options or resources change without a glslang version change.
constexpr u32 HostShaderCompilerRevision = 3;

constexpr TBuiltInResource DefaultTBuiltInResource = {
    .maxLights = 32,
    .maxClipPlanes = 6,
    .maxTextureUnits = 32,
    .maxTextureCoords = 32,
    .maxVertexAttribs = 64,
    .maxVertexUniformComponents = 4096,
    .maxVaryingFloats = 64,
    .maxVertexTextureImageUnits = 32,
    .maxCombinedTextureImageUnits = 80,
    .maxTextureImageUnits = 32,
    .maxFragmentUniformComponents = 4096,
    .maxDrawBuffers = 32,
    .maxVertexUniformVectors = 128,
    .maxVaryingVectors = 8,
    .maxFragmentUniformVectors = 16,
    .maxVertexOutputVectors = 16,
    .maxFragmentInputVectors = 15,
    .minProgramTexelOffset = -8,
    .maxProgramTexelOffset = 7,
    .maxClipDistances = 8,
    .maxComputeWorkGroupCountX = 65535,
    .maxComputeWorkGroupCountY = 65535,
    .maxComputeWorkGroupCountZ = 65535,
    .maxComputeWorkGroupSizeX = 1024,
    .maxComputeWorkGroupSizeY = 1024,
    .maxComputeWorkGroupSizeZ = 64,
    .maxComputeUniformComponents = 1024,
    .maxComputeTextureImageUnits = 16,
    .maxComputeImageUniforms = 8,
    .maxComputeAtomicCounters = 8,
    .maxComputeAtomicCounterBuffers = 1,
    .maxVaryingComponents = 60,
    .maxVertexOutputComponents = 64,
    .maxGeometryInputComponents = 64,
    .maxGeometryOutputComponents = 128,
    .maxFragmentInputComponents = 128,
    .maxImageUnits = 8,
    .maxCombinedImageUnitsAndFragmentOutputs = 8,
    .maxCombinedShaderOutputResources = 8,
    .maxImageSamples = 0,
    .maxVertexImageUniforms = 0,
    .maxTessControlImageUniforms = 0,
    .maxTessEvaluationImageUniforms = 0,
    .maxGeometryImageUniforms = 0,
    .maxFragmentImageUniforms = 8,
    .maxCombinedImageUniforms = 8,
    .maxGeometryTextureImageUnits = 16,
    .maxGeometryOutputVertices = 256,
    .maxGeometryTotalOutputComponents = 1024,
    .maxGeometryUniformComponents = 1024,
    .maxGeometryVaryingComponents = 64,
    .maxTessControlInputComponents = 128,
    .maxTessControlOutputComponents = 128,
    .maxTessControlTextureImageUnits = 16,
    .maxTessControlUniformComponents = 1024,
    .maxTessControlTotalOutputComponents = 4096,
    .maxTessEvaluationInputComponents = 128,
    .maxTessEvaluationOutputComponents = 128,
    .maxTessEvaluationTextureImageUnits = 16,
    .maxTessEvaluationUniformComponents = 1024,
    .maxTessPatchComponents = 120,
    .maxPatchVertices = 32,
    .maxTessGenLevel = 64,
    .maxViewports = 16,
    .maxVertexAtomicCounters = 0,
    .maxTessControlAtomicCounters = 0,
    .maxTessEvaluationAtomicCounters = 0,
    .maxGeometryAtomicCounters = 0,
    .maxFragmentAtomicCounters = 8,
    .maxCombinedAtomicCounters = 8,
    .maxAtomicCounterBindings = 1,
    .maxVertexAtomicCounterBuffers = 0,
    .maxTessControlAtomicCounterBuffers = 0,
    .maxTessEvaluationAtomicCounterBuffers = 0,
    .maxGeometryAtomicCounterBuffers = 0,
    .maxFragmentAtomicCounterBuffers = 1,
    .maxCombinedAtomicCounterBuffers = 1,
    .maxAtomicCounterBufferSize = 16384,
    .maxTransformFeedbackBuffers = 4,
    .maxTransformFeedbackInterleavedComponents = 64,
    .maxCullDistances = 8,
    .maxCombinedClipAndCullDistances = 8,
    .maxSamples = 4,
    .maxMeshOutputVerticesNV = 256,
    .maxMeshOutputPrimitivesNV = 512,
    .maxMeshWorkGroupSizeX_NV = 32,
    .maxMeshWorkGroupSizeY_NV = 1,
    .maxMeshWorkGroupSizeZ_NV = 1,
    .maxTaskWorkGroupSizeX_NV = 32,
    .maxTaskWorkGroupSizeY_NV = 1,
    .maxTaskWorkGroupSizeZ_NV = 1,
    .maxMeshViewCountNV = 4,
    .maxDualSourceDrawBuffersEXT = 1,
    .limits =
        TLimits{
            .nonInductiveForLoops = 1,
            .whileLoops = 1,
            .doWhileLoops = 1,
            .generalUniformIndexing = 1,
            .generalAttributeMatrixVectorIndexing = 1,
            .generalVaryingIndexing = 1,
            .generalSamplerIndexing = 1,
            .generalVariableIndexing = 1,
            .generalConstantMatrixVectorIndexing = 1,
        },
};

EShLanguage ToEshShaderStage(vk::ShaderStageFlagBits stage) {
    switch (stage) {
    case vk::ShaderStageFlagBits::eVertex:
        return EShLanguage::EShLangVertex;
    case vk::ShaderStageFlagBits::eGeometry:
        return EShLanguage::EShLangGeometry;
    case vk::ShaderStageFlagBits::eTessellationControl:
        return EShLanguage::EShLangTessControl;
    case vk::ShaderStageFlagBits::eTessellationEvaluation:
        return EShLanguage::EShLangTessEvaluation;
    case vk::ShaderStageFlagBits::eFragment:
        return EShLanguage::EShLangFragment;
    case vk::ShaderStageFlagBits::eCompute:
        return EShLanguage::EShLangCompute;
    default:
        UNREACHABLE_MSG("Unkown shader stage {}", vk::to_string(stage));
    }
    return EShLanguage::EShLangVertex;
}

bool InitializeCompiler() {
    static bool glslang_initialized = false;

    if (glslang_initialized) {
        return true;
    }

    if (!glslang::InitializeProcess()) {
        LOG_CRITICAL(Render_Vulkan, "Failed to initialize glslang shader compiler");
        return false;
    }

    std::atexit([]() { glslang::FinalizeProcess(); });

    glslang_initialized = true;
    return true;
}

template <typename T>
void HashValue(sha1::SHA1& hash, const T& value) {
    static_assert(std::is_trivially_copyable_v<T>);
    hash.processBytes(&value, sizeof(value));
}

void HashString(sha1::SHA1& hash, std::string_view value) {
    const auto size = static_cast<u64>(value.size());
    HashValue(hash, size);
    hash.processBytes(value.data(), value.size());
}

std::string FinishHash(sha1::SHA1& hash) {
    constexpr std::string_view HexDigits{"0123456789abcdef"};
    sha1::SHA1::digest8_t digest{};
    hash.getDigestBytes(digest);

    std::string result;
    result.reserve(sizeof(digest) * 2);
    for (const u8 value : digest) {
        result.push_back(HexDigits[value >> 4]);
        result.push_back(HexDigits[value & 0xf]);
    }
    return result;
}

std::string GetGeneration(const HostShaders::ShaderSource& source, vk::ShaderStageFlagBits stage) {
    sha1::SHA1 hash;
    HashString(hash, source.code);
    HashValue(hash, stage);
    HashValue(hash, HostShaderCompilerRevision);
    HashValue(hash, GLSLANG_VERSION_MAJOR);
    HashValue(hash, GLSLANG_VERSION_MINOR);
    HashValue(hash, GLSLANG_VERSION_PATCH);
    return FinishHash(hash);
}

std::string GetPermutation(std::span<const std::string> defines) {
    sha1::SHA1 hash;
    const auto count = static_cast<u64>(defines.size());
    HashValue(hash, count);
    for (const auto& define : defines) {
        HashString(hash, define);
    }
    return FinishHash(hash);
}

Storage::HostShaderCache& GetHostShaderCache() {
    static Storage::HostShaderCache cache{Common::FS::GetUserPath(Common::FS::PathType::CacheDir) /
                                          "host_shaders"};
    return cache;
}
} // Anonymous namespace

vk::ShaderModule Compile(const HostShaders::ShaderSource& source, vk::ShaderStageFlagBits stage,
                         vk::Device device, std::vector<std::string> defines,
                         bool use_raw_access_chains) {
    if (use_raw_access_chains) {
        defines.emplace_back("SHAD_NV_RAW_ACCESS_CHAINS=1");
    }
    const auto generation = GetGeneration(source, stage);
    const auto permutation = GetPermutation(defines);
    if (EmulatorSettings.IsPipelineCacheEnabled()) {
        if (auto spirv = GetHostShaderCache().Load(source.name, generation, permutation)) {
            return CompileSPV(*spirv, device);
        }
    }

    if (!InitializeCompiler()) {
        return {};
    }

    EProfile profile = ECoreProfile;
    EShMessages messages =
        static_cast<EShMessages>(EShMsgDefault | EShMsgSpvRules | EShMsgVulkanRules);
    EShLanguage lang = ToEshShaderStage(stage);

    const int default_version = 450;
    const char* pass_source_code = source.code.data();
    int pass_source_code_length = static_cast<int>(source.code.size());

    glslang::TShader shader{lang};
    shader.setEnvTarget(glslang::EShTargetSpv, glslang::EShTargetLanguageVersion::EShTargetSpv_1_3);
    shader.setStringsWithLengths(&pass_source_code, &pass_source_code_length, 1);

    std::string preambleString;
    std::vector<std::string> processes;

    for (auto& def : defines) {
        processes.push_back("define-macro ");
        processes.back().append(def);

        preambleString.append("#define ");
        if (const size_t equal = def.find_first_of("="); equal != def.npos) {
            def[equal] = ' ';
        }
        preambleString.append(def);
        preambleString.append("\n");
    }

    shader.setPreamble(preambleString.c_str());
    shader.addProcesses(processes);

    glslang::TShader::ForbidIncluder includer;

    std::string preprocessedStr;
    if (!shader.preprocess(&DefaultTBuiltInResource, default_version, profile, false, true,
                           messages, &preprocessedStr, includer)) [[unlikely]] {
        LOG_ERROR(Render_Vulkan,
                  "Shader preprocess error\n"
                  "Shader Info Log:\n"
                  "{}\n{}",
                  shader.getInfoLog(), shader.getInfoDebugLog());
        LOG_ERROR(Render_Vulkan, "Shader Source:\n{}", source.code);
        return {};
    }

    if (!shader.parse(&DefaultTBuiltInResource, default_version, profile, false, true, messages,
                      includer)) [[unlikely]] {
        LOG_ERROR(Render_Vulkan,
                  "Shader parse error\n"
                  "Shader Info Log:\n"
                  "{}\n{}",
                  shader.getInfoLog(), shader.getInfoDebugLog());
        LOG_ERROR(Render_Vulkan, "Shader Source:\n{}", source.code);
        return {};
    }

    glslang::TProgram program;
    program.addShader(&shader);
    if (!program.link(messages)) {
        LOG_ERROR(Render_Vulkan,
                  "Shader link error\n"
                  "Program Info Log:\n"
                  "{}\n{}",
                  program.getInfoLog(), program.getInfoDebugLog());
        return {};
    }

    glslang::TIntermediate* intermediate = program.getIntermediate(lang);
    std::vector<u32> out_code;
    spv::SpvBuildLogger logger;
    glslang::SpvOptions options;

    // Enable optimizations on the generated SPIR-V code.
    options.disableOptimizer = false;
    options.validate = false;
    options.optimizeSize = true;

    glslang::GlslangToSpv(*intermediate, out_code, &logger, &options);
    if (use_raw_access_chains) {
        Shader::Backend::SPIRV::ConvertRawAccessChains(out_code);
    }

    const std::string spv_messages = logger.getAllMessages();
    if (!spv_messages.empty()) {
        LOG_INFO(Render_Vulkan, "SPIR-V conversion messages: {}", spv_messages);
    }

    if (EmulatorSettings.IsPipelineCacheEnabled() &&
        !GetHostShaderCache().Store(source.name, generation, permutation, out_code)) {
        LOG_WARNING(Render_Vulkan, "Failed to cache host shader {}", source.name);
    }
    return CompileSPV(out_code, device);
}

namespace {

std::shared_mutex empty_fragment_mutex;
std::unordered_set<u64> empty_fragment_modules;

u64 ModuleKey(vk::ShaderModule module) {
    return reinterpret_cast<u64>(static_cast<VkShaderModule>(module));
}

/// Conservative SPIR-V scan. Only a single Fragment entry point whose body consists solely of
/// labels, plain branches/merges, debug line info and OpReturn is accepted. Any other instruction
/// in the entry function (loads, stores, calls, variables, demote/kill, atomics, image ops...) or
/// a DepthReplacing/StencilRefReplacing execution mode rejects the module.
bool IsEmptyFragmentSpirv(std::span<const u32> code) {
    constexpr u32 SpirvMagic = 0x07230203;
    constexpr u32 HeaderWords = 5;
    constexpr u32 OpLine = 8;
    constexpr u32 OpEntryPoint = 15;
    constexpr u32 OpExecutionMode = 16;
    constexpr u32 OpFunction = 54;
    constexpr u32 OpFunctionEnd = 56;
    constexpr u32 OpSelectionMerge = 247;
    constexpr u32 OpLabel = 248;
    constexpr u32 OpBranch = 249;
    constexpr u32 OpReturn = 253;
    constexpr u32 OpNoLine = 317;
    constexpr u32 ExecutionModelFragment = 4;
    constexpr u32 ExecutionModeDepthReplacing = 12;
    constexpr u32 ExecutionModeStencilRefReplacingEXT = 5027;

    if (code.size() < HeaderWords || code[0] != SpirvMagic) {
        return false;
    }
    u32 entry_point_count = 0;
    u32 entry_function = 0;
    bool entry_function_found = false;
    bool in_entry_function = false;
    size_t i = HeaderWords;
    while (i < code.size()) {
        const u32 op = code[i] & 0xFFFFu;
        const u32 count = code[i] >> 16;
        if (count == 0 || i + count > code.size()) {
            return false;
        }
        switch (op) {
        case OpEntryPoint:
            if (count < 4) {
                return false;
            }
            ++entry_point_count;
            if (code[i + 1] == ExecutionModelFragment) {
                entry_function = code[i + 2];
            }
            break;
        case OpExecutionMode:
            if (count >= 3 && (code[i + 2] == ExecutionModeDepthReplacing ||
                               code[i + 2] == ExecutionModeStencilRefReplacingEXT)) {
                return false;
            }
            break;
        case OpFunction:
            in_entry_function = entry_function != 0 && count >= 3 && code[i + 2] == entry_function;
            entry_function_found |= in_entry_function;
            break;
        case OpFunctionEnd:
            in_entry_function = false;
            break;
        default:
            if (in_entry_function) {
                switch (op) {
                case OpLine:
                case OpNoLine:
                case OpSelectionMerge:
                case OpLabel:
                case OpBranch:
                case OpReturn:
                    break;
                default:
                    return false;
                }
            }
            break;
        }
        i += count;
    }
    return entry_point_count == 1 && entry_function != 0 && entry_function_found;
}

} // namespace

vk::ShaderModule CompileSPV(std::span<const u32> code, vk::Device device) {
    const vk::ShaderModuleCreateInfo shader_info = {
        .codeSize = code.size() * sizeof(u32),
        .pCode = code.data(),
    };

    auto [module_result, module] = device.createShaderModule(shader_info);
    ASSERT_MSG(module_result == vk::Result::eSuccess, "Failed to compile SPIR-V shader: {}",
               vk::to_string(module_result));

    // Always refresh the entry for this handle: a destroyed module's handle may be reused.
    const bool is_empty_fragment = IsEmptyFragmentSpirv(code);
    {
        std::unique_lock lock{empty_fragment_mutex};
        if (is_empty_fragment) {
            empty_fragment_modules.insert(ModuleKey(module));
        } else {
            empty_fragment_modules.erase(ModuleKey(module));
        }
    }
    if (is_empty_fragment) {
        LOG_INFO(Render_Vulkan, "Fragment shader module {:#x} has an empty entry point",
                 ModuleKey(module));
    }
    return module;
}

bool IsEmptyFragmentModule(vk::ShaderModule module) {
    if (!module) {
        return false;
    }
    std::shared_lock lock{empty_fragment_mutex};
    return empty_fragment_modules.contains(ModuleKey(module));
}

} // namespace Vulkan
