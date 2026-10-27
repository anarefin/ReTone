//! Pure policy for sizing the llama-server context window.
//!
//! The context is not a flat cap but a per-machine, per-model budget: an
//! unbounded context once allocated a ~15 GB Metal-wired KV cache and froze a
//! 16 GB Mac, while a flat 16384 blocked every long request.
//! Everything here is arithmetic, so the memory guarantee is testable without
//! a GPU, a model file or a process.

use crate::gguf::{GgufMetadata, HeadCountKv};

/// Latin text tokenizes at roughly 3.5–4.2 chars/token on the BPEs used here;
/// 3 biases the estimate high, which is the safe direction.
pub const LATIN_CHARS_PER_TOKEN: usize = 3;

/// CJK runs about 1–1.5 chars/token; charging 1 keeps the estimate safe.
const CJK_RANGES: [(u32, u32); 9] = [
    (0x1100, 0x11ff),   // Hangul Jamo
    (0x3000, 0x303f),   // CJK symbols and punctuation
    (0x3040, 0x30ff),   // Hiragana + Katakana
    (0x3400, 0x4dbf),   // CJK Unified Ideographs Extension A
    (0x4e00, 0x9fff),   // CJK Unified Ideographs
    (0xac00, 0xd7af),   // Hangul Syllables
    (0xf900, 0xfaff),   // CJK Compatibility Ideographs
    (0xff00, 0xffef),   // Halfwidth and Fullwidth Forms
    (0x20000, 0x3ffff), // CJK Unified Ideographs Extensions B and beyond
];

fn is_cjk(c: char) -> bool {
    let code = c as u32;
    CJK_RANGES
        .iter()
        .any(|&(start, end)| code >= start && code <= end)
}

/// Cheap, deliberately high estimate of how many tokens `text` occupies. Only
/// picks the size a server starts at; a running server's `/tokenize` is exact.
pub fn estimate_tokens(text: &str) -> u64 {
    let (mut cjk, mut other) = (0u64, 0u64);
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(LATIN_CHARS_PER_TOKEN as u64)
}

/// K and V are cached as f16 by default.
pub const KV_ELEMENT_BYTES: u64 = 2;

fn head_count_kv(metadata: &GgufMetadata) -> Option<u64> {
    match metadata.head_count_kv.as_ref()? {
        // GGUF permits a per-layer array; the widest layer is the safe reading.
        HeadCountKv::PerLayer(values) => {
            let widest = values.iter().copied().fold(0.0f64, f64::max);
            (widest > 0.0).then_some(widest as u64)
        }
        HeadCountKv::Uniform(value) => (*value > 0.0).then_some(*value as u64),
    }
}

fn head_widths(metadata: &GgufMetadata) -> Option<(f64, f64)> {
    if let (Some(k), Some(v)) = (metadata.key_length, metadata.value_length)
        && k > 0.0
        && v > 0.0
    {
        return Some((k, v));
    }
    // llama.cpp's own fallback when the widths are not declared.
    if let (Some(embedding), Some(heads)) = (metadata.embedding_length, metadata.head_count)
        && embedding > 0.0
        && heads > 0.0
    {
        let width = embedding / heads;
        if width.fract() == 0.0 && width > 0.0 {
            return Some((width, width));
        }
    }
    None
}

/// Bytes of KV cache each token of context costs, from the model's own
/// architecture, assuming every layer is full attention (over-estimates
/// sliding-window and interleaved models, which errs safe). `None` when the
/// architecture cannot be read: callers fall back to the baseline context.
pub fn kv_bytes_per_token(metadata: Option<&GgufMetadata>) -> Option<u64> {
    let metadata = metadata?;
    let blocks = metadata.block_count.filter(|b| *b > 0.0)?;
    let heads = head_count_kv(metadata)?;
    let (k, v) = head_widths(metadata)?;
    Some((blocks * heads as f64 * (k + v)) as u64 * KV_ELEMENT_BYTES)
}

/// The historical cap, now the floor and the universal fallback.
pub const BASELINE_CONTEXT_SIZE: u64 = 16384;
/// Half the machine, minus a fixed slice for the OS.
pub const USABLE_MEMORY_FRACTION: f64 = 0.5;
pub const FIXED_OS_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Compute buffers also grow with context (measured, then doubled for margin).
pub const COMPUTE_FIXED_BYTES: u64 = 384 * 1024 * 1024;
pub const COMPUTE_BYTES_PER_TOKEN: u64 = 4096;
/// Recurrent state of hybrid models (Qwen3.5, LFM2), outside the KV cache.
pub const RECURRENT_STATE_BYTES: u64 = 512 * 1024 * 1024;
/// A discrete GPU's VRAM is not probed; stay modest until it is.
pub const UNVERIFIED_GPU_CEILING: u64 = 65536;
/// Contexts are aligned down so near-identical requests land on one value.
pub const CONTEXT_ALIGNMENT: u64 = 512;

const MIB: f64 = 1024.0 * 1024.0;
pub const MIN_CACHE_RAM_MIB: u64 = 512;
pub const MAX_CACHE_RAM_MIB: u64 = 2048;

/// Why the ceiling has the value it has (logged; asserted by tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingReason {
    NoMemoryInfo,
    UnknownKvCost,
    Floor,
    TrainedContextBound,
    GpuCap,
    MemoryBound,
}

impl CeilingReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoMemoryInfo => "no-memory-info",
            Self::UnknownKvCost => "unknown-kv-cost",
            Self::Floor => "floor",
            Self::TrainedContextBound => "trained-context-bound",
            Self::GpuCap => "gpu-cap",
            Self::MemoryBound => "memory-bound",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextCeiling {
    pub ceiling: u64,
    pub reason: CeilingReason,
    /// `--cache-ram` bound so the prompt cache cannot eat the KV budget.
    pub cache_ram_mib: u64,
}

fn fallback_ceiling(reason: CeilingReason) -> ContextCeiling {
    ContextCeiling {
        ceiling: BASELINE_CONTEXT_SIZE,
        reason,
        cache_ram_mib: MIN_CACHE_RAM_MIB,
    }
}

/// Inputs of [`resolve_context_ceiling`]. Memory is `f64` so unusable readings
/// (NaN, negative) can be expressed, as in the JS.
#[derive(Debug, Clone, Copy, Default)]
pub struct CeilingInputs {
    pub total_memory_bytes: f64,
    pub weights_bytes: u64,
    pub draft_weights_bytes: u64,
    pub kv_bytes_per_token: Option<u64>,
    pub has_recurrent_state: bool,
    pub trained_context_tokens: u64,
    pub discrete_gpu_unverified: bool,
}

/// The largest context this machine may allocate for this model. Every path
/// that cannot answer confidently returns [`BASELINE_CONTEXT_SIZE`].
pub fn resolve_context_ceiling(input: CeilingInputs) -> ContextCeiling {
    if !input.total_memory_bytes.is_finite() || input.total_memory_bytes <= 0.0 {
        return fallback_ceiling(CeilingReason::NoMemoryInfo);
    }
    let kv = match input.kv_bytes_per_token {
        Some(kv) if kv > 0 => kv,
        _ => return fallback_ceiling(CeilingReason::UnknownKvCost),
    };

    let budget = input.total_memory_bytes * USABLE_MEMORY_FRACTION - FIXED_OS_RESERVE_BYTES as f64;
    let kv_budget = budget
        - input.weights_bytes as f64
        - input.draft_weights_bytes as f64
        - COMPUTE_FIXED_BYTES as f64
        - if input.has_recurrent_state {
            RECURRENT_STATE_BYTES as f64
        } else {
            0.0
        };
    if kv_budget <= 0.0 {
        return fallback_ceiling(CeilingReason::Floor);
    }

    let cache_ram_mib =
        ((kv_budget / 4.0 / MIB).floor() as u64).clamp(MIN_CACHE_RAM_MIB, MAX_CACHE_RAM_MIB);

    let per_token = (kv + COMPUTE_BYTES_PER_TOKEN) as f64;
    let affordable =
        ((kv_budget / per_token).floor() as u64 / CONTEXT_ALIGNMENT) * CONTEXT_ALIGNMENT;

    let mut hard_max = if input.trained_context_tokens > 0 {
        input.trained_context_tokens
    } else {
        u64::MAX
    };
    let mut hard_max_reason = CeilingReason::TrainedContextBound;
    if input.discrete_gpu_unverified && UNVERIFIED_GPU_CEILING < hard_max {
        hard_max = UNVERIFIED_GPU_CEILING;
        hard_max_reason = CeilingReason::GpuCap;
    }

    let ceiling = affordable.min(hard_max);
    if ceiling <= BASELINE_CONTEXT_SIZE {
        return fallback_ceiling(CeilingReason::Floor);
    }
    ContextCeiling {
        ceiling,
        reason: if ceiling == hard_max {
            hard_max_reason
        } else {
            CeilingReason::MemoryBound
        },
        cache_ram_mib,
    }
}

/// Context rungs: bounds the restarts a session can cause to one per rung.
pub const CONTEXT_STEPS: [u64; 5] = [16384, 32768, 65536, 131072, 262144];

/// The context to start (or grow) the server at for a request needing
/// `needed` tokens, given a caller floor and the machine's ceiling. The
/// ceiling always wins, and need not be on a rung.
pub fn resolve_context_size(needed: u64, floor: u64, ceiling: u64) -> u64 {
    let target = needed.max(floor).max(BASELINE_CONTEXT_SIZE);
    let rung = CONTEXT_STEPS
        .iter()
        .copied()
        .find(|step| *step >= target)
        .unwrap_or(u64::MAX);
    let bounded = rung.min(ceiling.max(BASELINE_CONTEXT_SIZE));
    bounded.max(BASELINE_CONTEXT_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    fn meta(blocks: f64, heads_kv: HeadCountKv, k: Option<f64>, v: Option<f64>) -> GgufMetadata {
        GgufMetadata {
            architecture: "llama".into(),
            block_count: Some(blocks),
            head_count_kv: Some(heads_kv),
            key_length: k,
            value_length: v,
            ..GgufMetadata::default()
        }
    }

    // --- estimate_tokens

    #[test]
    fn estimate_counts_latin_at_three_chars_per_token() {
        assert_eq!(estimate_tokens("abcdef"), 2);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_over_estimates_a_real_transcript() {
        let estimate = estimate_tokens(&"a".repeat(85171));
        assert!(estimate > 20473);
        assert!(estimate < 20473 * 2);
    }

    #[test]
    fn estimate_counts_cjk_at_one_token_per_code_point() {
        assert_eq!(estimate_tokens("会議の議事録"), 6);
        assert_eq!(estimate_tokens("ひらがな"), 4);
        assert_eq!(estimate_tokens("한국어"), 3);
    }

    #[test]
    fn estimate_charges_cjk_more_than_latin() {
        assert!(estimate_tokens(&"会".repeat(300)) > estimate_tokens(&"x".repeat(300)));
    }

    #[test]
    fn estimate_sums_mixed_scripts() {
        assert_eq!(estimate_tokens("会議abcdef"), 4);
    }

    // --- kv_bytes_per_token

    #[test]
    fn kv_reproduces_the_measured_cost_of_the_1203_model() {
        let m = meta(28.0, HeadCountKv::Uniform(8.0), Some(128.0), Some(128.0));
        let bytes = kv_bytes_per_token(Some(&m)).unwrap();
        assert_eq!(bytes, 114688);
        assert_eq!(bytes * 131072, 15032385536);
    }

    #[test]
    fn kv_is_anti_correlated_with_file_size() {
        let qwen9b = kv_bytes_per_token(Some(&meta(
            33.0,
            HeadCountKv::Uniform(4.0),
            Some(256.0),
            Some(256.0),
        )))
        .unwrap();
        let llama3b = kv_bytes_per_token(Some(&meta(
            28.0,
            HeadCountKv::Uniform(8.0),
            Some(128.0),
            Some(128.0),
        )))
        .unwrap();
        assert_eq!(qwen9b, 135168);
        assert!(llama3b < qwen9b);
        assert!(qwen9b > 32768);
    }

    #[test]
    fn kv_takes_the_widest_layer_of_a_per_layer_array() {
        let m = meta(
            4.0,
            HeadCountKv::PerLayer(vec![2.0, 8.0, 2.0, 2.0]),
            Some(128.0),
            Some(128.0),
        );
        assert_eq!(kv_bytes_per_token(Some(&m)), Some(4 * 8 * 256 * 2));
    }

    #[test]
    fn kv_derives_head_width_from_the_embedding() {
        let m = GgufMetadata {
            architecture: "llama".into(),
            block_count: Some(28.0),
            head_count_kv: Some(HeadCountKv::Uniform(8.0)),
            embedding_length: Some(3072.0),
            head_count: Some(24.0),
            ..GgufMetadata::default()
        };
        assert_eq!(kv_bytes_per_token(Some(&m)), Some(114688));
    }

    #[test]
    fn kv_returns_none_rather_than_guessing() {
        assert_eq!(kv_bytes_per_token(None), None);
        assert_eq!(kv_bytes_per_token(Some(&GgufMetadata::default())), None);
        let blocks_only = GgufMetadata {
            block_count: Some(28.0),
            ..GgufMetadata::default()
        };
        assert_eq!(kv_bytes_per_token(Some(&blocks_only)), None);
        let no_widths = GgufMetadata {
            block_count: Some(28.0),
            head_count_kv: Some(HeadCountKv::Uniform(8.0)),
            ..GgufMetadata::default()
        };
        assert_eq!(kv_bytes_per_token(Some(&no_widths)), None);
        let zero_blocks = GgufMetadata {
            block_count: Some(0.0),
            head_count_kv: Some(HeadCountKv::Uniform(8.0)),
            key_length: Some(128.0),
            ..GgufMetadata::default()
        };
        assert_eq!(kv_bytes_per_token(Some(&zero_blocks)), None);
    }

    // --- resolve_context_ceiling

    fn llama_3_2_3b(total: f64) -> CeilingInputs {
        CeilingInputs {
            total_memory_bytes: total,
            kv_bytes_per_token: Some(114688),
            weights_bytes: 2168958976,
            trained_context_tokens: 131072,
            ..CeilingInputs::default()
        }
    }

    fn qwen_3_5_9b(total: f64) -> CeilingInputs {
        CeilingInputs {
            total_memory_bytes: total,
            kv_bytes_per_token: Some(135168),
            weights_bytes: 6169341984,
            trained_context_tokens: 262144,
            has_recurrent_state: true,
            ..CeilingInputs::default()
        }
    }

    #[test]
    fn regression_1203_a_16_gb_mac_stays_far_below_the_panic() {
        let input = llama_3_2_3b(16.0 * GIB);
        let ceiling = resolve_context_ceiling(input).ceiling;
        assert_eq!(ceiling, 32256);
        let footprint = input.weights_bytes + ceiling * 114688 + COMPUTE_FIXED_BYTES;
        assert!((footprint as f64) < 0.4 * 16.0 * GIB);
        assert!(ceiling > BASELINE_CONTEXT_SIZE);
    }

    #[test]
    fn the_ceiling_tracks_kv_cost_not_model_size() {
        let cheap = resolve_context_ceiling(CeilingInputs {
            total_memory_bytes: 48.0 * GIB,
            kv_bytes_per_token: Some(32768),
            weights_bytes: 5 * (GIB as u64),
            trained_context_tokens: 262144,
            ..CeilingInputs::default()
        });
        let expensive = resolve_context_ceiling(CeilingInputs {
            total_memory_bytes: 48.0 * GIB,
            kv_bytes_per_token: Some(393216),
            weights_bytes: 5 * (GIB as u64),
            trained_context_tokens: 131072,
            ..CeilingInputs::default()
        });
        assert!(cheap.ceiling > expensive.ceiling * 3);
    }

    #[test]
    fn the_reported_48_gb_machine_holds_the_failing_transcript() {
        let ceiling = resolve_context_ceiling(qwen_3_5_9b(48.0 * GIB)).ceiling;
        assert_eq!(ceiling, 118272);
        assert!(ceiling > 20514 + 2048);
    }

    #[test]
    fn an_unreadable_architecture_falls_back_to_the_baseline() {
        let result = resolve_context_ceiling(CeilingInputs {
            kv_bytes_per_token: None,
            ..llama_3_2_3b(48.0 * GIB)
        });
        assert_eq!(result.ceiling, BASELINE_CONTEXT_SIZE);
        assert_eq!(result.reason, CeilingReason::UnknownKvCost);
    }

    #[test]
    fn unusable_memory_readings_fall_back_to_the_baseline() {
        for total in [0.0, f64::NAN, -1.0, f64::INFINITY] {
            let result = resolve_context_ceiling(llama_3_2_3b(total));
            assert_eq!(result.ceiling, BASELINE_CONTEXT_SIZE, "total={total}");
            assert_eq!(result.reason, CeilingReason::NoMemoryInfo);
        }
    }

    #[test]
    fn a_machine_too_small_for_headroom_keeps_the_baseline() {
        let result = resolve_context_ceiling(llama_3_2_3b(8.0 * GIB));
        assert_eq!(result.ceiling, BASELINE_CONTEXT_SIZE);
        assert_eq!(result.reason, CeilingReason::Floor);
    }

    #[test]
    fn the_ceiling_never_exceeds_the_trained_context() {
        let result = resolve_context_ceiling(CeilingInputs {
            total_memory_bytes: 512.0 * GIB,
            kv_bytes_per_token: Some(32768),
            weights_bytes: 5 * (GIB as u64),
            trained_context_tokens: 32768,
            ..CeilingInputs::default()
        });
        assert_eq!(result.ceiling, 32768);
        assert_eq!(result.reason, CeilingReason::TrainedContextBound);
    }

    #[test]
    fn an_unmeasured_discrete_gpu_is_capped() {
        let result = resolve_context_ceiling(CeilingInputs {
            total_memory_bytes: 128.0 * GIB,
            kv_bytes_per_token: Some(32768),
            weights_bytes: 3 * (GIB as u64),
            trained_context_tokens: 262144,
            discrete_gpu_unverified: true,
            ..CeilingInputs::default()
        });
        assert!(result.ceiling <= 65536);
    }

    #[test]
    fn the_ceiling_moves_monotonically_with_each_input() {
        let base = llama_3_2_3b(32.0 * GIB);
        let at = |f: &dyn Fn(&mut CeilingInputs)| {
            let mut input = base;
            f(&mut input);
            resolve_context_ceiling(input).ceiling
        };
        let baseline = at(&|_| {});
        assert!(at(&|i| i.total_memory_bytes = 64.0 * GIB) >= baseline);
        assert!(at(&|i| i.weights_bytes = 6 * (GIB as u64)) <= baseline);
        assert!(at(&|i| i.kv_bytes_per_token = Some(393216)) <= baseline);
        assert!(at(&|i| i.has_recurrent_state = true) <= baseline);
        assert!(at(&|i| i.draft_weights_bytes = 2 * (GIB as u64)) <= baseline);
    }

    #[test]
    fn the_prompt_cache_is_bounded() {
        for total in [8.0, 16.0, 48.0, 128.0] {
            let mib = resolve_context_ceiling(llama_3_2_3b(total * GIB)).cache_ram_mib;
            assert!((512..=2048).contains(&mib), "{mib} at {total} GiB");
        }
    }

    // --- resolve_context_size

    #[test]
    fn short_requests_stay_on_the_baseline_rung() {
        assert_eq!(resolve_context_size(800, 0, 131072), 16384);
        assert_eq!(resolve_context_size(0, 0, 131072), 16384);
    }

    #[test]
    fn grows_to_the_smallest_rung_that_fits() {
        assert_eq!(resolve_context_size(23074, 0, 131072), 32768);
        assert_eq!(resolve_context_size(16385, 0, 131072), 32768);
        assert_eq!(resolve_context_size(32768, 0, 131072), 32768);
        assert_eq!(resolve_context_size(32769, 0, 131072), 65536);
    }

    #[test]
    fn near_identical_requests_share_a_rung() {
        assert_eq!(
            resolve_context_size(20600, 0, 131072),
            resolve_context_size(21100, 0, 131072)
        );
    }

    #[test]
    fn never_returns_more_than_the_ceiling() {
        assert_eq!(resolve_context_size(40000, 0, 32256), 32256);
        assert_eq!(resolve_context_size(1_000_000, 0, 32256), 32256);
        assert_eq!(resolve_context_size(100000, 0, 65536), 65536);
    }

    #[test]
    fn honours_a_floor_without_breaching_the_ceiling() {
        assert_eq!(resolve_context_size(500, 32768, 131072), 32768);
        assert_eq!(resolve_context_size(500, 262144, 32256), 32256);
    }

    #[test]
    fn never_drops_below_the_baseline() {
        assert_eq!(resolve_context_size(100, 0, 4096), 16384);
    }
}
