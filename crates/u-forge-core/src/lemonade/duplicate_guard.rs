//! Guard against duplicate llamacpp model names across CPU and GPU backends.
//!
//! Lemonade Server cannot load two models with the same name on different
//! llamacpp backends simultaneously.  This is a known server-side limitation:
//! if `embed-gemma.gguf` is loaded on `rocm` and the client requests it again
//! on `cpu`, the server returns an error.
//!
//! FLM models (`*-FLM`) are never in conflict because their names are distinct
//! from their llamacpp GGUF counterparts.  This guard only needs to examine
//! `llamacpp` recipe selections.
//!
//! When Lemonade fixes this upstream, delete this file and remove the one call
//! site in the discovery flow.

use anyhow::{bail, Result};

use super::selector::SelectedModel;

/// Detects and resolves duplicate llamacpp model names across backends.
pub struct DuplicateGuard;

/// Priority order for llamacpp backends: higher wins.
fn backend_priority(backend: Option<&str>) -> u8 {
    match backend {
        Some("rocm") => 3,
        Some("vulkan") => 2,
        Some("metal") => 1,
        _ => 0, // "cpu" or None
    }
}

impl DuplicateGuard {
    /// Returns `Err` if any `model_id` appears more than once across different
    /// llamacpp backends in `selections`.
    ///
    /// Non-llamacpp recipes are ignored — FLM, whispercpp, and kokoro models
    /// can never conflict with each other or with llamacpp models.
    pub fn check(selections: &[SelectedModel]) -> Result<()> {
        use std::collections::HashMap;

        let mut seen: HashMap<&str, &str> = HashMap::new();

        for sel in selections {
            if sel.recipe != "llamacpp" {
                continue;
            }
            let backend = sel.backend.as_deref().unwrap_or("cpu");
            if let Some(existing_backend) = seen.insert(&sel.model_id, backend) {
                if existing_backend != backend {
                    bail!(
                        "Duplicate llamacpp model '{}' selected for both '{}' and '{}' backends. \
                         Call DuplicateGuard::deduplicate() to resolve.",
                        sel.model_id,
                        existing_backend,
                        backend
                    );
                }
            }
        }

        Ok(())
    }

    /// Remove lower-priority duplicates in-place.
    ///
    /// For each set of llamacpp selections with the same `model_id`, keeps the
    /// entry with the highest-priority backend (`rocm` > `vulkan` > `metal` >
    /// `cpu`) and removes all others.
    ///
    /// Non-llamacpp entries are never removed.
    pub fn deduplicate(selections: &mut Vec<SelectedModel>) {
        use std::collections::HashMap;

        // For each conflicting model_id, find the index of the entry to keep
        // (highest priority backend).
        let mut best: HashMap<String, usize> = HashMap::new();

        for (i, sel) in selections.iter().enumerate() {
            if sel.recipe != "llamacpp" {
                continue;
            }
            let priority = backend_priority(sel.backend.as_deref());
            best.entry(sel.model_id.clone())
                .and_modify(|best_idx| {
                    let current_best_priority =
                        backend_priority(selections[*best_idx].backend.as_deref());
                    if priority > current_best_priority {
                        *best_idx = i;
                    }
                })
                .or_insert(i);
        }

        // Collect indices to remove: llamacpp entries that lost their conflict.
        let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (i, sel) in selections.iter().enumerate() {
            if sel.recipe != "llamacpp" {
                continue;
            }
            if best.get(&sel.model_id) != Some(&i) {
                remove.insert(i);
            }
        }

        if !remove.is_empty() {
            let mut idx = 0;
            selections.retain(|_| {
                let keep = !remove.contains(&idx);
                idx += 1;
                keep
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lemonade::selector::{QualityTier, SelectedModel};
    use crate::lemonade::load::ModelLoadOptions;

    fn sel(model_id: &str, recipe: &str, backend: Option<&str>) -> SelectedModel {
        SelectedModel {
            model_id: model_id.to_string(),
            recipe: recipe.to_string(),
            backend: backend.map(String::from),
            load_opts: ModelLoadOptions::default(),
            quality_tier: QualityTier::NotApplicable,
        }
    }

    // ── check() ───────────────────────────────────────────────────────────────

    #[test]
    fn test_check_no_conflict_passes() {
        let selections = vec![
            sel("embed-gemma.gguf", "llamacpp", Some("rocm")),
            sel("llm.gguf", "llamacpp", Some("rocm")),
            sel("embed-flm", "flm", None),
        ];
        assert!(DuplicateGuard::check(&selections).is_ok());
    }

    #[test]
    fn test_check_detects_same_model_different_backends() {
        let selections = vec![
            sel("embed-gemma.gguf", "llamacpp", Some("rocm")),
            sel("embed-gemma.gguf", "llamacpp", Some("cpu")),
        ];
        assert!(DuplicateGuard::check(&selections).is_err());
    }

    #[test]
    fn test_check_same_model_same_backend_is_not_a_conflict() {
        // Two workers for the same model+backend is unusual but not a conflict.
        let selections = vec![
            sel("embed-gemma.gguf", "llamacpp", Some("rocm")),
            sel("embed-gemma.gguf", "llamacpp", Some("rocm")),
        ];
        assert!(DuplicateGuard::check(&selections).is_ok());
    }

    #[test]
    fn test_check_flm_never_conflicts_with_llamacpp() {
        // FLM and llamacpp may share a base name — not a conflict.
        let selections = vec![
            sel("embed-gemma-300m-FLM", "flm", None),
            sel("embed-gemma-300m-FLM", "llamacpp", Some("rocm")), // different recipe
        ];
        assert!(DuplicateGuard::check(&selections).is_ok());
    }

    #[test]
    fn test_check_non_llamacpp_never_conflicts() {
        let selections = vec![
            sel("whisper-turbo", "whispercpp", None),
            sel("whisper-turbo", "flm", None),
            sel("kokoro-v1", "kokoro", None),
        ];
        assert!(DuplicateGuard::check(&selections).is_ok());
    }

    // ── deduplicate() ─────────────────────────────────────────────────────────

    #[test]
    fn test_deduplicate_keeps_rocm_over_cpu() {
        let mut selections = vec![
            sel("embed-gemma.gguf", "llamacpp", Some("cpu")),
            sel("embed-gemma.gguf", "llamacpp", Some("rocm")),
        ];
        DuplicateGuard::deduplicate(&mut selections);
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].backend.as_deref(), Some("rocm"));
    }

    #[test]
    fn test_deduplicate_keeps_rocm_over_vulkan() {
        let mut selections = vec![
            sel("llm.gguf", "llamacpp", Some("vulkan")),
            sel("llm.gguf", "llamacpp", Some("rocm")),
        ];
        DuplicateGuard::deduplicate(&mut selections);
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].backend.as_deref(), Some("rocm"));
    }

    #[test]
    fn test_deduplicate_keeps_vulkan_over_cpu() {
        let mut selections = vec![
            sel("llm.gguf", "llamacpp", Some("cpu")),
            sel("llm.gguf", "llamacpp", Some("vulkan")),
        ];
        DuplicateGuard::deduplicate(&mut selections);
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].backend.as_deref(), Some("vulkan"));
    }

    #[test]
    fn test_deduplicate_preserves_non_llamacpp_entries() {
        let mut selections = vec![
            sel("embed-gemma.gguf", "llamacpp", Some("rocm")),
            sel("embed-gemma.gguf", "llamacpp", Some("cpu")),
            sel("embed-gemma-FLM", "flm", None),   // must survive
            sel("kokoro-v1", "kokoro", None),       // must survive
        ];
        DuplicateGuard::deduplicate(&mut selections);
        assert_eq!(selections.len(), 3);

        let ids: Vec<&str> = selections.iter().map(|m| m.model_id.as_str()).collect();
        assert!(ids.contains(&"embed-gemma-FLM"));
        assert!(ids.contains(&"kokoro-v1"));

        let llamacpp: Vec<_> = selections.iter().filter(|m| m.recipe == "llamacpp").collect();
        assert_eq!(llamacpp.len(), 1);
        assert_eq!(llamacpp[0].backend.as_deref(), Some("rocm"));
    }

    #[test]
    fn test_deduplicate_no_conflict_is_noop() {
        let mut selections = vec![
            sel("model-a.gguf", "llamacpp", Some("rocm")),
            sel("model-b.gguf", "llamacpp", Some("cpu")),
        ];
        DuplicateGuard::deduplicate(&mut selections);
        assert_eq!(selections.len(), 2);
    }

    #[test]
    fn test_deduplicate_three_way_conflict_keeps_highest() {
        let mut selections = vec![
            sel("model.gguf", "llamacpp", Some("cpu")),
            sel("model.gguf", "llamacpp", Some("vulkan")),
            sel("model.gguf", "llamacpp", Some("rocm")),
        ];
        DuplicateGuard::deduplicate(&mut selections);
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].backend.as_deref(), Some("rocm"));
    }

    #[test]
    fn test_check_then_deduplicate_roundtrip() {
        let mut selections = vec![
            sel("embed.gguf", "llamacpp", Some("rocm")),
            sel("embed.gguf", "llamacpp", Some("cpu")),
        ];
        assert!(DuplicateGuard::check(&selections).is_err());
        DuplicateGuard::deduplicate(&mut selections);
        assert!(DuplicateGuard::check(&selections).is_ok());
        assert_eq!(selections[0].backend.as_deref(), Some("rocm"));
    }
}
